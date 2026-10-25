//! The queued serial peripheral interface (QSMRM 4; MC68331UM 6.3): a
//! queue of up to 16 transfers from the QSPI RAM.
//!
//! As a master each goes to the host's peripheral with its PCS code,
//! timed from SPCR0's baud and SPCR1's delays; as a slave each takes
//! the bits an external master, the host, clocks while it holds PCS0/SS
//! low.

use std::any::Any;
use std::fmt;

use mc683xx_cpu32::cpu::state::Save;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::frequency::Frequency;

use super::port::PortQs;
use crate::peripheral::ClockPhase;
use crate::peripheral::ClockPolarity;
use crate::peripheral::QspiClock;
use crate::peripheral::QspiTransfer;
use crate::peripheral::SpiPeripheral;

/// The QSPI RAM's bytes: receive, transmit and command (QSMRM 4.3.6).
pub(super) const RAM_BYTES: usize = 0x50;
/// The receive RAM's offset in the QSPI RAM, a word an entry.
const RECEIVE_RAM: usize = 0x00;
/// The transmit RAM's, a word an entry.
const TRANSMIT_RAM: usize = 0x20;
/// The command RAM's, a byte an entry.
const COMMAND_RAM: usize = 0x40;
/// Queue entries, $0 to $F (QSMRM 4.3.3, ENDQP).
const ENTRIES: u8 = 16;

/// SPCR0 MSTR: master mode (QSMRM 4.3.1).
const SPCR0_MSTR: u16 = 1 << 15;
/// SPCR0 WOMQ: the QSPI's pins are open drain.
const SPCR0_WOMQ: u16 = 1 << 14;
/// SPCR0 BITS, bits 13:10: bits per transfer with BITSE.
const SPCR0_BITS_SHIFT: u16 = 10;
/// SPCR0 BITS, unshifted.
const SPCR0_BITS_MASK: u16 = 0xf;
/// SPCR0 CPOL: SCK idles high.
const SPCR0_CPOL: u16 = 1 << 9;
/// SPCR0 CPHA: data changes on SCK's leading edge.
const SPCR0_CPHA: u16 = 1 << 8;
/// SPCR0 SPBR, bits 7:0: SCK = system clock / (2 x SPBR).
const SPCR0_SPBR_MASK: u16 = 0xff;
/// The least SPBR that runs the baud rate generator; 0 and 1 stop it.
const SPBR_LEAST: u16 = 2;
/// SPCR0 out of reset: CPHA, SPBR 4 (QSMRM 4.3.1).
const SPCR0_RESET: u16 = 0x0104;
/// SPCR1 SPE: the QSPI is enabled (QSMRM 4.3.2).
const SPCR1_SPE: u16 = 1 << 15;
/// SPCR1 DSCKL, bits 14:8: PCS to SCK delay with DSCK.
const SPCR1_DSCKL_SHIFT: u16 = 8;
/// SPCR1 DSCKL, unshifted.
const SPCR1_DSCKL_MASK: u16 = 0x7f;
/// SPCR1 DTL, bits 7:0: delay after transfer with DT.
const SPCR1_DTL_MASK: u16 = 0xff;
/// SPCR1 out of reset: DSCKL 4, DTL 4 (QSMRM 4.3.2).
const SPCR1_RESET: u16 = 0x0404;
/// SPCR2 SPIFIE: SPIF interrupts (QSMRM 4.3.3).
const SPCR2_SPIFIE: u16 = 1 << 15;
/// SPCR2 WREN: wraparound mode.
const SPCR2_WREN: u16 = 1 << 14;
/// SPCR2 WRTO: wrap to NEWQP, else to $0.
const SPCR2_WRTO: u16 = 1 << 13;
/// SPCR2 ENDQP, bits 11:8.
const SPCR2_ENDQP_SHIFT: u16 = 8;
/// A queue pointer's bits.
const POINTER_MASK: u16 = 0xf;
/// SPCR2's implemented bits.
const SPCR2_WRITABLE: u16 = 0xef0f;
/// SPCR3 LOOPQ: received data is the transmitted data (QSMRM 4.3.4).
const SPCR3_LOOPQ: u8 = 1 << 2;
/// SPCR3 HMIE: HALTA and MODF interrupts.
const SPCR3_HMIE: u8 = 1 << 1;
/// SPCR3 HALT: halt at the next queue boundary.
const SPCR3_HALT: u8 = 1;
/// SPCR3's implemented bits.
const SPCR3_WRITABLE: u8 = 0b111;
/// SPSR SPIF: the queue is finished (QSMRM 4.3.5).
pub(super) const SPSR_SPIF: u8 = 1 << 7;
/// SPSR MODF: mode fault.
pub(super) const SPSR_MODF: u8 = 1 << 6;
/// SPSR HALTA: halted.
pub(super) const SPSR_HALTA: u8 = 1 << 5;
/// SPSR's flags, which a write of zero clears once a read armed them.
const SPSR_FLAGS: u8 = SPSR_SPIF | SPSR_MODF | SPSR_HALTA;
/// A command byte's CONT: PCS stays driven after the transfer (QSMRM
/// 4.3.6.3).
const COMMAND_CONT: u8 = 1 << 7;
/// A command's BITSE: SPCR0's BITS, else 8 bits.
const COMMAND_BITSE: u8 = 1 << 6;
/// A command's DT: SPCR1's DTL delay after, else the standard delay.
const COMMAND_DT: u8 = 1 << 5;
/// A command's DSCK: SPCR1's DSCKL before SCK, else half an SCK
/// period.
const COMMAND_DSCK: u8 = 1 << 4;
/// A command's PCS\[3:0\].
const COMMAND_PCS_MASK: u8 = 0xf;
/// Bits in a transfer without BITSE, or with a reserved BITS (QSMRM
/// Table 4-2).
const DEFAULT_BITS: u8 = 8;
/// Bits for BITS 0, the most in a transfer (QSMRM Table 4-2).
pub const MOST_BITS: u8 = 16;
/// The least BITS that is not reserved.
const LEAST_BITS: u16 = 8;
/// DSCKL 0's delay, in system clocks (QSMRM 4.3.2, NOTE).
const DSCKL_ZERO_CLOCKS: u64 = 128;
/// DSCKL 1 times as 2 (QSMRM 4.3.2, NOTE).
const DSCKL_LEAST: u64 = 2;
/// System clocks per unit of DTL (QSMRM equation 4-4).
const DTL_CLOCKS: u64 = 32;
/// DTL 0 counts as 256.
const DTL_ZERO: u64 = 256;
/// The standard delay after a transfer, 17 system clocks (QSMRM
/// equation 4-5).
const STANDARD_DELAY_CLOCKS: u64 = 17;
/// System clocks in an SCK period per unit of SPBR (QSMRM equation
/// 4-1).
const SCK_CLOCKS_PER_SPBR: u64 = 2;

/// Whether SPCR0 `value` sets WOMQ: the QSPI's pins are open drain
/// (QSMRM 4.3.1).
pub(super) const fn wired_or(value: u16) -> bool {
    value & SPCR0_WOMQ != 0
}

/// What the QSPI as a slave drove on MISO while the host's master
/// shifted a word (QSMRM 4.4.2.1), right-aligned, most significant bit
/// first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Shifted {
    /// The levels it drove, 0 where it drove none.
    pub miso: u16,
    /// The bits it drove: unselected, halted, MISO not its output, or
    /// a high bit with WOMQ, it drives none.
    pub driven: u16,
}

impl Shifted {
    /// Bit `bit` drove `miso`, as
    /// [`Qsm::clock_slave`](super::Qsm::clock_slave) returned it.
    ///
    /// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
    #[doc(hidden)]
    pub fn record(&mut self, bit: u8, miso: Option<bool>) {
        if let Some(level) = miso {
            self.driven |= 1 << bit;
            self.miso |= u16::from(level) << bit;
        }
    }
}

/// A QSPI peripheral the chip holds, which the host can get back by
/// type.  It says whether it saves ([`Save`]), as a chip's peripherals do:
/// the chip's snapshot holds it.
///
/// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
#[doc(hidden)]
pub trait AnySpiPeripheral: SpiPeripheral + Save + Any {}

impl<T: SpiPeripheral + Save + Any> AnySpiPeripheral for T {}

/// Where a queue entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Phase {
    /// PCS to SCK delay and the serial transfer, ending at the time.
    Shifting(Cycles),
    /// The delay after the transfer, ending at the time.
    Delay(Cycles),
}

/// The queue's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Queue {
    /// SPE clear.
    Idle,
    /// SPE set, but the master cannot transfer: SPBR below 2.  It
    /// starts at the entry when it can.
    Waiting(u8),
    /// A slave's entry, taking the bits the external master clocks
    /// (QSMRM 4.4.2.1).
    Slave {
        /// The entry.
        pointer: u8,
        /// Its transmit word, as read at its start.
        out: u16,
        /// The bits received so far, right-aligned.
        received: u16,
        /// How many.
        count: u8,
    },
    /// Running an entry.
    Entry {
        /// The entry.
        pointer: u8,
        /// Its command byte, as read at its start.
        command: u8,
        /// Its transmit word, as read at its start.
        out: u16,
        /// Its bits.
        bits: u8,
        /// Where it is.
        phase: Phase,
    },
    /// Halted at a queue boundary by HALT, before this entry.
    Halted(u8),
}

/// An SPCR2 write waiting for the transfer in progress to end (QSMRM
/// 4.3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Buffered {
    /// The value.
    value: u16,
    /// Whether the write reached NEWQP, which restarts the queue there
    /// even at the same value.
    new_pointer: bool,
}

/// The QSPI's state.
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct Qspi {
    /// SPCR0.
    spcr0: u16,
    /// SPCR1.
    spcr1: u16,
    /// SPCR2 as read.
    spcr2: u16,
    /// A write of SPCR2 not yet in effect.
    buffered: Option<Buffered>,
    /// SPCR3.
    spcr3: u8,
    /// SPSR's flags.
    flags: u8,
    /// The flags a read of SPSR armed to clear.
    armed: u8,
    /// SPSR's CPTQP.
    completed: u8,
    /// The QSPI RAM.
    #[serde(with = "mc683xx_cpu32::cpu::state::byte_array")]
    pub(super) ram: [u8; RAM_BYTES],
    /// The queue.
    queue: Queue,
    /// The PCS code the QSPI drives, during an entry or after one with
    /// CONT.
    drive: Option<u8>,
    /// The PCS\[3:0\] levels the peripheral last saw.
    selects: u8,
    /// The host's peripheral: not state, kept across a restore.
    #[serde(skip)]
    peripheral: Option<Box<dyn AnySpiPeripheral>>,
    /// The system clock's frequency, as the board last set it.
    #[serde(with = "mc683xx_cpu32::frequency::hertz")]
    system_clock: Frequency,
    /// The serial clock the peripheral was last told.
    told: Option<QspiClock>,
}

/// The serial clock SPCR0 `spcr0` gives with SPBR `spbr` and the
/// system clock at `system_clock` (QSMRM 4.3.1).
const fn serial_clock(spcr0: u16, spbr: u64, system_clock: Frequency) -> QspiClock {
    QspiClock {
        polarity: if spcr0 & SPCR0_CPOL == 0 {
            ClockPolarity::IdleLow
        } else {
            ClockPolarity::IdleHigh
        },
        phase: if spcr0 & SPCR0_CPHA == 0 {
            ClockPhase::CaptureLeading
        } else {
            ClockPhase::ChangeLeading
        },
        period: Cycles(SCK_CLOCKS_PER_SPBR * spbr),
        system_clock,
    }
}

impl QspiClock {
    /// The serial clock out of reset, SPCR0 at its reset value (CPOL
    /// clear, CPHA set, SPBR 4: QSMRM 4.3.1) with the system clock at
    /// `system_clock`.  The system clock out of reset is the board's
    /// (8.388608 MHz from a 32.768-kHz reference, MC68331UM 4.3), so the
    /// caller gives it.
    #[must_use]
    pub const fn reset(system_clock: Frequency) -> Self {
        serial_clock(
            SPCR0_RESET,
            (SPCR0_RESET & SPCR0_SPBR_MASK) as u64,
            system_clock,
        )
    }
}

impl fmt::Debug for Qspi {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Qspi")
            .field("spcr0", &self.spcr0)
            .field("spcr1", &self.spcr1)
            .field("spcr2", &self.spcr2)
            .field("spcr3", &self.spcr3)
            .field("flags", &self.flags)
            .field("queue", &self.queue)
            .field("peripheral", &self.peripheral.is_some())
            .finish_non_exhaustive()
    }
}

impl Qspi {
    /// The QSPI out of reset, its RAM zero (QSMRM leaves it undefined;
    /// PLAN.md), PCS\[3:0\] at `selects`, the system clock at
    /// `system_clock`.
    pub(super) const fn new(selects: u8, system_clock: Frequency) -> Self {
        Self {
            spcr0: SPCR0_RESET,
            spcr1: SPCR1_RESET,
            spcr2: 0,
            buffered: None,
            spcr3: 0,
            flags: 0,
            armed: 0,
            completed: 0,
            ram: [0; RAM_BYTES],
            queue: Queue::Idle,
            drive: None,
            selects,
            peripheral: None,
            system_clock,
            told: None,
        }
    }

    /// The QSPI after a reset: the RAM, the peripheral, and what it was
    /// told, stay.
    pub(super) fn reset(&mut self, port: PortQs, now: Cycles) {
        let ram = self.ram;
        let peripheral = self.peripheral.take();
        *self = Self {
            ram,
            peripheral,
            told: self.told,
            ..Self::new(self.selects, self.system_clock)
        };
        self.notify(port, now);
    }

    /// RESET reaches the host's peripheral at `now`.
    pub(super) fn reset_peripheral(&mut self, now: Cycles) {
        if let Some(peripheral) = &mut self.peripheral {
            peripheral.reset(now);
        }
    }

    /// The board's system clock is now at `frequency`.
    pub(super) const fn set_system_clock(&mut self, frequency: Frequency) {
        self.system_clock = frequency;
    }

    /// The serial clock SPCR0 and the system clock give with SPBR
    /// `spbr` (QSMRM 4.3.1).
    const fn clock(&self, spbr: u64) -> QspiClock {
        serial_clock(self.spcr0, spbr, self.system_clock)
    }

    /// The peripheral learns the serial clock at `now` if it changed.
    fn tell_clock(&mut self, spbr: u64, now: Cycles) {
        let clock = self.clock(spbr);
        if self.told != Some(clock)
            && let Some(peripheral) = &mut self.peripheral
        {
            self.told = Some(clock);
            peripheral.clock(clock, now);
        }
    }

    /// The host's peripheral.
    pub(super) fn peripheral(&self) -> Option<&dyn AnySpiPeripheral> {
        self.peripheral.as_deref()
    }

    /// The host's peripheral, to change.
    pub(super) fn peripheral_mut(&mut self) -> Option<&mut (dyn AnySpiPeripheral + 'static)> {
        self.peripheral.as_deref_mut()
    }

    /// The host wires `peripheral` to the QSPI's pins.
    pub(super) fn attach(&mut self, peripheral: Box<dyn AnySpiPeripheral>) {
        self.peripheral = Some(peripheral);
        self.told = None;
    }

    /// Takes `saved`'s state, keeping the host's peripheral.
    /// Whether a snapshot's QSPI is one a run could leave: implemented
    /// register bits only, queue pointers to the 16 entries, a
    /// transfer's bits 1 to 16, PCS codes of four bits.
    pub(super) fn is_valid(&self) -> bool {
        let entry = |pointer: u8| pointer < ENTRIES;
        let bits = |bits: u8| (1..=MOST_BITS).contains(&bits);
        let queue = match self.queue {
            Queue::Idle => true,
            Queue::Waiting(pointer) | Queue::Halted(pointer) => entry(pointer),
            Queue::Slave { pointer, count, .. } => entry(pointer) && count <= MOST_BITS,
            Queue::Entry {
                pointer, bits: n, ..
            } => entry(pointer) && bits(n),
        };
        queue
            && self.spcr2 & !SPCR2_WRITABLE == 0
            && self
                .buffered
                .is_none_or(|buffered| buffered.value & !SPCR2_WRITABLE == 0)
            && self.spcr3 & !SPCR3_WRITABLE == 0
            && (self.flags | self.armed) & !SPSR_FLAGS == 0
            && entry(self.completed)
            && self.drive.is_none_or(|code| code & !COMMAND_PCS_MASK == 0)
            && self.selects & !COMMAND_PCS_MASK == 0
    }

    /// Halts the queue before an entry past the 16, as no run can.
    #[cfg(test)]
    pub(super) const fn halt_past_the_queue(&mut self) {
        self.queue = Queue::Halted(ENTRIES);
    }

    /// Takes `saved`'s state, keeping the host's peripheral, which a
    /// snapshot does not hold.
    pub(super) fn restore(&mut self, saved: Self) {
        let peripheral = self.peripheral.take();
        *self = Self {
            peripheral,
            ..saved
        };
    }

    /// SPCR0.
    pub(super) const fn spcr0(&self) -> u16 {
        self.spcr0
    }

    /// SPCR1.
    pub(super) const fn spcr1(&self) -> u16 {
        self.spcr1
    }

    /// SPCR2, not a buffered write (QSMRM 4.3.3).
    pub(super) const fn spcr2(&self) -> u16 {
        self.spcr2
    }

    /// SPCR3.
    pub(super) const fn spcr3(&self) -> u8 {
        self.spcr3
    }

    /// SPSR.
    pub(super) const fn spsr(&self) -> u8 {
        self.flags | self.completed
    }

    /// SPSR as read: the read arms its set flags to clear.
    pub(super) const fn read_spsr(&mut self) -> u8 {
        self.armed = self.flags;
        self.spsr()
    }

    /// Whether SPE is set.
    const fn enabled(&self) -> bool {
        self.spcr1 & SPCR1_SPE != 0
    }

    /// NEWQP.
    const fn new_pointer(&self) -> u8 {
        (self.spcr2 & POINTER_MASK) as u8
    }

    /// The PCS code on the pins the QSPI has, while SPE is set.
    pub(super) const fn code(&self) -> Option<u8> {
        if self.enabled() { self.drive } else { None }
    }

    /// The peripheral sees PCS\[3:0\] at `now` if their levels changed.
    pub(super) fn notify(&mut self, port: PortQs, now: Cycles) {
        let levels = port.chip_selects(self.code());
        if levels != self.selects {
            self.selects = levels;
            if let Some(peripheral) = &mut self.peripheral {
                peripheral.chip_selects(levels, now);
            }
        }
    }

    /// SPCR0 written at `now`.
    pub(super) fn write_spcr0(&mut self, value: u16, port: PortQs, now: Cycles) {
        self.spcr0 = value;
        match self.queue {
            Queue::Waiting(pointer) => self.start(pointer, port, now),
            Queue::Slave {
                pointer, count: 0, ..
            } if self.master() => self.start(pointer, port, now),
            _ => {}
        }
    }

    /// SPCR1 written at `now`: SPE from 0 starts the queue at NEWQP
    /// (QSMRM 4.4.1.1); SPE cleared stops it at once, a transfer in
    /// progress lost (4.3.2).
    pub(super) fn write_spcr1(&mut self, value: u16, port: PortQs, now: Cycles) {
        let was = self.enabled();
        self.spcr1 = value;
        match (was, self.enabled()) {
            (false, true) => {
                self.apply_buffered();
                self.start(self.new_pointer(), port, now);
            }
            (true, false) => self.stop(port, now),
            _ => {}
        }
    }

    /// SPCR2 written at `now`, `new_pointer` if the write reached
    /// NEWQP: during an entry it waits for the entry's end.
    pub(super) fn write_spcr2(&mut self, value: u16, new_pointer: bool, port: PortQs, now: Cycles) {
        let value = value & SPCR2_WRITABLE;
        let new_pointer = new_pointer || self.buffered.is_some_and(|buffered| buffered.new_pointer);
        self.buffered = Some(Buffered { value, new_pointer });
        let transferring = match self.queue {
            Queue::Entry { .. } => true,
            Queue::Slave { count, .. } => count != 0,
            Queue::Idle | Queue::Waiting(_) | Queue::Halted(_) => false,
        };
        if !transferring {
            let moved = self.apply_buffered();
            match self.queue {
                Queue::Halted(_) if moved => self.queue = Queue::Halted(self.new_pointer()),
                Queue::Waiting(_) | Queue::Slave { .. } if moved => {
                    self.start(self.new_pointer(), port, now);
                }
                _ => {}
            }
        }
    }

    /// The buffered SPCR2 takes effect; whether it moves the queue to
    /// NEWQP.
    const fn apply_buffered(&mut self) -> bool {
        match self.buffered.take() {
            Some(buffered) => {
                self.spcr2 = buffered.value;
                buffered.new_pointer
            }
            None => false,
        }
    }

    /// SPCR3 written at `now`: HALT cleared resumes a halted queue
    /// (QSMRM 4.3.4).
    pub(super) fn write_spcr3(&mut self, value: u8, port: PortQs, now: Cycles) {
        self.spcr3 = value & SPCR3_WRITABLE;
        if let Queue::Halted(pointer) = self.queue
            && self.spcr3 & SPCR3_HALT == 0
        {
            self.start(pointer, port, now);
        }
    }

    /// SPSR written: a zero clears a flag a read armed (QSMRM 4.3.5);
    /// CPTQP is the QSPI's.
    pub(super) const fn write_spsr(&mut self, value: u8) {
        let cleared = self.armed & !value & SPSR_FLAGS;
        self.flags &= !cleared;
        self.armed &= !cleared;
    }

    /// Checks for a mode fault at `now`: a master whose PCS0/SS input
    /// the board holds low clears SPE (QSMRM 4.3.5, MODF).
    pub(super) fn check_mode_fault(&mut self, port: PortQs, now: Cycles) -> bool {
        let fault =
            self.enabled() && self.master() && port.slave_select_input() && port.slave_select_low();
        if fault {
            self.flags |= SPSR_MODF;
            self.spcr1 &= !SPCR1_SPE;
            self.stop(port, now);
        }
        fault
    }

    /// The queue stops at `now`: SPE is clear and PCS returns to
    /// PORTQS.
    fn stop(&mut self, port: PortQs, now: Cycles) {
        self.queue = Queue::Idle;
        self.drive = None;
        self.notify(port, now);
    }

    /// SPBR, if the baud rate generator runs.
    const fn baud_divisor(&self) -> Option<u64> {
        let spbr = self.spcr0 & SPCR0_SPBR_MASK;
        if spbr >= SPBR_LEAST {
            Some(spbr as u64)
        } else {
            None
        }
    }

    /// Whether MSTR is set (QSMRM 4.3.1).
    const fn master(&self) -> bool {
        self.spcr0 & SPCR0_MSTR != 0
    }

    /// Bits in a master's transfer of `command`: SPCR0's BITS with
    /// BITSE, else 8 (QSMRM Table 4-2).
    const fn bits(&self, command: u8) -> u8 {
        if command & COMMAND_BITSE == 0 {
            DEFAULT_BITS
        } else {
            self.spcr0_bits()
        }
    }

    /// Bits per transfer as SPCR0's BITS sets them, a reserved value 8
    /// (QSMRM 4.3.1, Table 4-2); a slave's every transfer.
    const fn spcr0_bits(&self) -> u8 {
        match self.spcr0 >> SPCR0_BITS_SHIFT & SPCR0_BITS_MASK {
            0 => MOST_BITS,
            bits if bits >= LEAST_BITS => bits as u8,
            _ => DEFAULT_BITS,
        }
    }

    /// The PCS to SCK delay and the transfer of `command`, `bits` long,
    /// in system clocks (QSMRM 4.3.2, equations 4-1 and 4-3).
    const fn shift_clocks(&self, command: u8, bits: u8, spbr: u64) -> u64 {
        let before = if command & COMMAND_DSCK == 0 {
            // half an SCK period
            spbr
        } else {
            match (self.spcr1 >> SPCR1_DSCKL_SHIFT & SPCR1_DSCKL_MASK) as u64 {
                0 => DSCKL_ZERO_CLOCKS,
                dsckl if dsckl < DSCKL_LEAST => DSCKL_LEAST,
                dsckl => dsckl,
            }
        };
        before + bits as u64 * SCK_CLOCKS_PER_SPBR * spbr
    }

    /// The delay after a transfer of `command`, in system clocks
    /// (QSMRM equations 4-4, 4-5).
    const fn delay_clocks(&self, command: u8) -> u64 {
        if command & COMMAND_DT == 0 {
            return STANDARD_DELAY_CLOCKS;
        }
        match (self.spcr1 & SPCR1_DTL_MASK) as u64 {
            0 => DTL_CLOCKS * DTL_ZERO,
            dtl => DTL_CLOCKS * dtl,
        }
    }

    /// Entry `pointer`'s transmit word.
    const fn transmit_word(&self, pointer: u8) -> u16 {
        let at = TRANSMIT_RAM + 2 * pointer as usize;
        u16::from_be_bytes([self.ram[at], self.ram[at + 1]])
    }

    /// Entry `pointer` received `word`, `bits` long: right-aligned in
    /// receive RAM, the unused bits zero (QSMRM 4.3.6.1), or the word
    /// sent with LOOPQ (4.3.4), and CPTQP moves.
    fn store_received(&mut self, pointer: u8, word: u16, out: u16, bits: u8) {
        let mask = u16::MAX >> (MOST_BITS - bits);
        let word = if self.spcr3 & SPCR3_LOOPQ != 0 {
            out
        } else {
            word
        };
        let at = RECEIVE_RAM + 2 * usize::from(pointer);
        self.ram[at..at + 2].copy_from_slice(&(word & mask).to_be_bytes());
        self.completed = pointer;
    }

    /// Entry `pointer` starts at `now`: as a master its command and
    /// transmit word read, its PCS code driven (QSMRM Figure 4-4); as a
    /// slave its transmit word read, PCS left to PORTQS (Figure 4-5,
    /// 4.4.2.1).
    fn start(&mut self, pointer: u8, port: PortQs, now: Cycles) {
        if !self.master() {
            self.drive = None;
            self.notify(port, now);
            self.queue = Queue::Slave {
                pointer,
                out: self.transmit_word(pointer),
                received: 0,
                count: 0,
            };
            return;
        }
        if self.check_mode_fault(port, now) {
            return;
        }
        let Some(spbr) = self.baud_divisor() else {
            self.queue = Queue::Waiting(pointer);
            return;
        };
        let command = self.ram[COMMAND_RAM + usize::from(pointer)];
        let out = self.transmit_word(pointer);
        let bits = self.bits(command);
        self.tell_clock(spbr, now);
        self.drive = Some(command & COMMAND_PCS_MASK);
        self.notify(port, now);
        self.queue = Queue::Entry {
            pointer,
            command,
            out,
            bits,
            phase: Phase::Shifting(now + Cycles(self.shift_clocks(command, bits, spbr))),
        };
    }

    /// The end of the queue entry's phase in progress: where it next
    /// changes on its own, seen or not.
    pub(super) const fn next_phase_end(&self) -> Option<Cycles> {
        match self.queue {
            Queue::Entry {
                phase: Phase::Shifting(end) | Phase::Delay(end),
                ..
            } => Some(end),
            _ => None,
        }
    }

    /// When it next changes in a way the CPU or the peripheral could
    /// see before the CPU next reads or writes the QSM, which brings it
    /// to that time (PLAN.md, Peripherals): an entry's end that sets
    /// SPIF with SPIFIE, halts, or takes a buffered SPCR2; and
    /// with a peripheral whose calls cannot be deferred, each call.
    /// Entries between, with nothing to see, run in a batch when it is
    /// brought up to date.  `port` gives the pins.
    pub(super) fn next_event(&self, port: PortQs) -> Option<Cycles> {
        let Queue::Entry {
            pointer,
            command,
            phase,
            ..
        } = self.queue
        else {
            return None;
        };
        let watched = self
            .peripheral
            .as_ref()
            .is_some_and(|peripheral| !peripheral.deferrable());
        let mut end = match phase {
            Phase::Shifting(end) if watched => return Some(end),
            Phase::Shifting(end) => end + Cycles(self.delay_clocks(command)),
            Phase::Delay(end) => end,
        };
        let mut pointer = pointer;
        let mut finished = self.flags & SPSR_SPIF != 0;
        // A pass of the queue sets SPIF at most once and changes
        // nothing else unseen, so past two passes nothing new shows.
        for _ in 0..=2 * ENTRIES {
            // `finish` at `end`, then `start` of the next entry
            if self.buffered.is_some() || self.spcr3 & SPCR3_HALT != 0 {
                return Some(end);
            }
            let last = u16::from(pointer) == self.spcr2 >> SPCR2_ENDQP_SHIFT & POINTER_MASK;
            if last {
                if !finished && self.spcr2 & SPCR2_SPIFIE != 0 {
                    return Some(end);
                }
                finished = true;
                if self.spcr2 & SPCR2_WREN == 0 {
                    // it stops, PCS back to PORTQS
                    return (watched && self.releases(port)).then_some(end);
                }
            }
            pointer = if last && self.spcr2 & SPCR2_WRTO != 0 {
                self.new_pointer()
            } else if last {
                0
            } else {
                (pointer + 1) % ENTRIES
            };
            if !self.master() {
                // a slave from here, PCS back to PORTQS
                return (watched && self.releases(port)).then_some(end);
            }
            // the pins and MSTR change only by writes, each checked
            debug_assert!(
                !(port.slave_select_input() && port.slave_select_low()),
                "a mode fault not taken"
            );
            let spbr = self.baud_divisor()?;
            let command = self.ram[COMMAND_RAM + usize::from(pointer)];
            if watched
                && (self.told != Some(self.clock(spbr))
                    || port.chip_selects(Some(command & COMMAND_PCS_MASK)) != self.selects)
            {
                return Some(end);
            }
            let shifted = end + Cycles(self.shift_clocks(command, self.bits(command), spbr));
            if watched {
                return Some(shifted);
            }
            end = shifted + Cycles(self.delay_clocks(command));
        }
        None
    }

    /// Whether PCS\[3:0\] returning to PORTQS changes their levels, and
    /// so calls the peripheral: not after an entry without CONT, which
    /// returned them at its transfer (QSMRM 4.3.6.3).
    fn releases(&self, port: PortQs) -> bool {
        port.chip_selects(None) != self.selects
    }

    /// The entry's phase ends `off` later: its clock was off that long.
    pub(super) fn delay(&mut self, off: Cycles) {
        if let Queue::Entry {
            phase: Phase::Shifting(end) | Phase::Delay(end),
            ..
        } = &mut self.queue
        {
            *end += off;
        }
    }

    /// Brings it to `now`, its entries in order.
    pub(super) fn advance(&mut self, port: PortQs, now: Cycles) {
        while let Queue::Entry {
            pointer,
            command,
            out,
            bits,
            phase,
        } = self.queue
        {
            match phase {
                Phase::Shifting(end) if end <= now => {
                    self.transfer(pointer, command, out, bits, port, end);
                }
                Phase::Delay(end) if end <= now => self.finish(pointer, port, end),
                _ => break,
            }
        }
    }

    /// Entry `pointer`'s serial transfer ends at `now`: the peripheral
    /// shifts, the received bits go to receive RAM right-aligned, CPTQP
    /// moves, and without CONT PCS returns to PORTQS (QSMRM 4.4.1.1).
    fn transfer(
        &mut self,
        pointer: u8,
        command: u8,
        out: u16,
        bits: u8,
        port: PortQs,
        now: Cycles,
    ) {
        let out = out & u16::MAX >> (MOST_BITS - bits);
        // told at the entry's start; a peripheral attached since, the
        // clock SPCR0 gives now
        let clock = self
            .told
            .unwrap_or_else(|| self.clock(u64::from(self.spcr0 & SPCR0_SPBR_MASK)));
        let transfer = QspiTransfer::new(pointer, self.selects, out, bits, clock, now);
        let received = self
            .peripheral
            .as_mut()
            .map_or(0, |peripheral| peripheral.transfer(transfer));
        self.store_received(pointer, received, out, bits);
        if command & COMMAND_CONT == 0 {
            self.drive = None;
            self.notify(port, now);
        }
        self.queue = Queue::Entry {
            pointer,
            command,
            out,
            bits,
            phase: Phase::Delay(now + Cycles(self.delay_clocks(command))),
        };
    }

    /// Whether the QSPI is a slave the external master selects: SPE
    /// set, MSTR clear, and PCS0/SS an input the board holds low
    /// (QSMRM 4.4.2.1; MC68331UM Table 6-1).
    const fn slave_selected(&self, port: PortQs) -> bool {
        self.enabled() && !self.master() && port.slave_select_input() && port.slave_select_low()
    }

    /// The external master clocks one bit at `now`, `mosi` on MOSI,
    /// while the QSPI is a slave (QSMRM 4.4.2.1): the level the QSPI
    /// drives on MISO for it, if it drives one.  Unselected, halted,
    /// or past the end of a queue without wraparound, it ignores the
    /// bit.  At BITS bits the entry's word goes to receive RAM and the
    /// queue moves on as a master's does (Figure 4-5).
    pub(super) fn clock_slave(&mut self, mosi: bool, port: PortQs, now: Cycles) -> Option<bool> {
        if !self.slave_selected(port) {
            return None;
        }
        let Queue::Slave {
            pointer,
            out,
            received,
            count,
        } = self.queue
        else {
            return None;
        };
        let bits = self.spcr0_bits();
        // the transmit word right-aligned, its bits most significant
        // first (QSMRM 4.3.6.2, 4.4.2.1)
        let level = count < bits && out >> (bits - 1 - count) & 1 != 0;
        let miso = port.miso(level);
        if !port.sck_input() {
            return miso;
        }
        let received = received << 1 | u16::from(mosi && port.mosi_input());
        let count = count + 1;
        if count < bits {
            self.queue = Queue::Slave {
                pointer,
                out,
                received,
                count,
            };
        } else {
            self.store_received(pointer, received, out, bits);
            self.finish(pointer, port, now);
        }
        miso
    }

    /// Entry `pointer` ends at `now`: at ENDQP SPIF sets and the queue
    /// stops or wraps; a buffered SPCR2 takes effect; HALT halts it
    /// (QSMRM Figures 4-4 and 4-5, 4.3.3, 4.3.4).
    fn finish(&mut self, pointer: u8, port: PortQs, now: Cycles) {
        let last = u16::from(pointer) == self.spcr2 >> SPCR2_ENDQP_SHIFT & POINTER_MASK;
        let moved = self.apply_buffered();
        let halt = self.spcr3 & SPCR3_HALT != 0;
        if last {
            self.flags |= SPSR_SPIF;
            if self.spcr2 & SPCR2_WREN == 0 || halt {
                if halt {
                    self.flags |= SPSR_HALTA;
                }
                self.spcr1 &= !SPCR1_SPE;
                self.stop(port, now);
                return;
            }
        }
        let next = if moved || last && self.spcr2 & SPCR2_WRTO != 0 {
            self.new_pointer()
        } else if last {
            0
        } else {
            (pointer + 1) % ENTRIES
        };
        if halt {
            self.flags |= SPSR_HALTA;
            self.queue = Queue::Halted(next);
        } else {
            self.start(next, port, now);
        }
    }

    /// Whether the QSPI requests an interrupt: SPIF with SPIFIE, HALTA
    /// or MODF with HMIE (QSMRM 4.3.3, 4.3.4).
    pub(super) const fn requesting(&self) -> bool {
        self.spcr2 & SPCR2_SPIFIE != 0 && self.flags & SPSR_SPIF != 0
            || self.spcr3 & SPCR3_HMIE != 0 && self.flags & (SPSR_HALTA | SPSR_MODF) != 0
    }
}
