//! The queued serial module (QSMRM; MC68331UM 6): its global and pin
//! registers, the SCI and the QSPI.
//!
//! The chip routes its register cycles here and arbitrates its two
//! interrupt requests; the host sends and takes SCI frames, wires a
//! QSPI peripheral, and drives the port's input pins.

mod port;
pub mod qspi;
pub mod sci;

use std::any::Any;
use std::mem;

use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_cpu32::interrupt::InterruptLevel;

use self::port::PortQs;
use self::qspi::AnySpiPeripheral;
use self::qspi::Qspi;
use self::sci::Frame;
use self::sci::Sci;
use self::sci::Transmitted;
use crate::module::Block;
use crate::module::InterruptRequest;
use crate::module::Module;
use crate::module::Peek;
use crate::module::RegisterAccess;
use crate::peripheral::Timed;
use crate::pins::PortLevels;

/// The QSM's register block and RAM, $YFFC00 to $YFFDFF (MC68331UM
/// Table D-1).
pub const BLOCK: Block = Block {
    offset: 0xc00,
    length: 0x200,
};

/// Register offsets in the block (MC68331UM Table D-13; QSMRM Table
/// 3-1), each a word's even address.
const QSMCR: u32 = 0x00;
/// QTEST, for factory test.
const QTEST: u32 = 0x02;
/// QILR in the upper byte, QIVR in the lower.
const QILR_QIVR: u32 = 0x04;
/// SCCR0.
const SCCR0: u32 = 0x08;
/// SCCR1.
const SCCR1: u32 = 0x0a;
/// SCSR.
const SCSR: u32 = 0x0c;
/// SCDR.
const SCDR: u32 = 0x0e;
/// PORTQS, the lower byte.
const PORTQS: u32 = 0x14;
/// PQSPAR in the upper byte, DDRQS in the lower.
const PQSPAR_DDRQS: u32 = 0x16;
/// SPCR0.
const SPCR0: u32 = 0x18;
/// SPCR1.
const SPCR1: u32 = 0x1a;
/// SPCR2.
const SPCR2: u32 = 0x1c;
/// SPCR3 in the upper byte, SPSR in the lower.
const SPCR3_SPSR: u32 = 0x1e;
/// The QSPI RAM: receive, transmit and command, $YFFD00 to $YFFD4F
/// (QSMRM 4.3.6).
const RAM: u32 = 0x100;
/// The end of the QSPI RAM.
const RAM_END: u32 = RAM + qspi::RAM_BYTES as u32;
/// The global registers, supervisor-only whatever SUPV (MC68331UM
/// Table D-13, "S").
const SUPERVISOR_ONLY_END: u32 = 0x06;

/// QSMCR STOP, FRZ1, FRZ0, SUPV and IARB, which a write sets (QSMRM
/// 3.2.1).
const QSMCR_WRITABLE: u16 = 0xe08f;
/// QSMCR SUPV: the assignable registers are supervisor-only.
const QSMCR_SUPV: u16 = 1 << 7;
/// QSMCR IARB, bits 3:0.
const QSMCR_IARB_MASK: u16 = 0xf;
/// QSMCR out of reset: SUPV (QSMRM 3.2.1; MC68331UM D.4.1).
const QSMCR_RESET: u16 = 0x0080;
/// QILR ILQSPI, bits 5:3 (QSMRM 3.2.3).
const ILQSPI_SHIFT: u8 = 3;
/// A QILR level field, unshifted.
const LEVEL_MASK: u8 = 0b111;
/// QILR's implemented bits.
const QILR_WRITABLE: u8 = 0x3f;
/// QIVR INTV0: 1 for the QSPI's vector, 0 for the SCI's; reads one,
/// writes have no effect (QSMRM 3.2.4).
const INTV0: u8 = 1;
/// QIVR out of reset: $0F, the uninitialized interrupt vector.
const QIVR_RESET: u8 = 0x0f;
/// The format of the QSM's snapshot ([`Module::save`]).
const STATE_VERSION: u32 = 4;

/// The QSM's state.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Qsm {
    /// QSMCR.
    qsmcr: u16,
    /// QILR.
    qilr: u8,
    /// QIVR's INTV\[7:1\].
    qivr: u8,
    /// Port QS.
    port: PortQs,
    /// The SCI.
    sci: Sci,
    /// The QSPI.
    qspi: Qspi,
    /// When LPSTOP turned its clock off, while it is off (MC68331UM
    /// 4.3.4): the SCI and the QSPI make no progress.
    low_power: Option<Cycles>,
    /// What port QS drives, each change with its time, until the host
    /// takes them.
    output_changes: Vec<(Cycles, PortLevels)>,
}

impl Qsm {
    /// The QSM out of reset, with no QSPI peripheral, the system clock
    /// at `system_clock`.
    #[must_use]
    pub const fn new(system_clock: Frequency) -> Self {
        let port = PortQs::new();
        Self {
            qsmcr: QSMCR_RESET,
            qilr: 0,
            qivr: QIVR_RESET & !INTV0,
            port,
            sci: Sci::new(),
            qspi: Qspi::new(port.chip_selects(None), system_clock),
            low_power: None,
            output_changes: Vec::new(),
        }
    }

    /// LPSTOP turns its clock off at `now` (MC68331UM 4.3.4): the SCI
    /// and the QSPI stop where they are, frames on RXD pass unseen, and
    /// the pins' changes wait for the clock ([PLAN.md], LPSTOP).
    ///
    /// [PLAN.md]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md
    pub fn stop_clock(&mut self, now: Cycles) {
        self.advance(now);
        self.low_power.get_or_insert(now);
    }

    /// A wake turns its clock on at `now`: the SCI and the QSPI go on
    /// from where they stopped, every time they were waiting for later
    /// by the clock's time off, and see the pins as they are.
    pub fn start_clock(&mut self, now: Cycles) {
        if let Some(stopped) = self.low_power.take() {
            let off = stopped.until(now);
            self.sci.delay(off);
            self.qspi.delay(off);
            let before = self.port_outputs();
            self.pins_changed(now);
            self.record_outputs(before, now);
        }
    }

    /// Whether LPSTOP has its clock off.
    const fn clock_off(&self) -> bool {
        self.low_power.is_some()
    }

    /// The board's system clock is now at `frequency`, as SYNCR sets
    /// it: the QSPI's peripheral learns its SCK rate from it (QSMRM
    /// 4.3.1).
    pub const fn set_system_clock(&mut self, frequency: Frequency) {
        self.qspi.set_system_clock(frequency);
    }

    /// RESET reaches the host's QSPI peripheral at `now`
    /// ([`SpiPeripheral::reset`](crate::peripheral::SpiPeripheral::reset)).
    ///
    /// The QSM's own reset does not: the chip calls this for the resets
    /// the board wires to the peripheral.
    pub fn reset_peripheral(&mut self, now: Cycles) {
        self.qspi.reset_peripheral(now);
    }

    /// The host wires `peripheral` to the QSPI's pins, replacing any.
    pub fn attach(&mut self, peripheral: Box<dyn AnySpiPeripheral>) {
        self.qspi.attach(peripheral);
    }

    /// The QSPI peripheral, if it is a `D`.
    #[must_use]
    pub fn peripheral<D: AnySpiPeripheral>(&self) -> Option<&D> {
        let peripheral: &dyn Any = self.attached()?;
        peripheral.downcast_ref()
    }

    /// The QSPI peripheral, to change, if it is a `D`.
    pub fn peripheral_mut<D: AnySpiPeripheral>(&mut self) -> Option<&mut D> {
        let peripheral: &mut dyn Any = self.attached_mut()?;
        peripheral.downcast_mut()
    }

    /// The QSPI peripheral, whatever it is: the chip saves and restores
    /// it with its own snapshot, not the QSM's.
    #[must_use]
    pub fn attached(&self) -> Option<&dyn AnySpiPeripheral> {
        self.qspi.peripheral()
    }

    /// The QSPI peripheral, whatever it is, to change.
    pub fn attached_mut(&mut self) -> Option<&mut (dyn AnySpiPeripheral + 'static)> {
        self.qspi.peripheral_mut()
    }

    /// The host sends `frame` on RXD at `now`, after any it sent before
    /// that have not yet arrived.
    pub fn receive(&mut self, frame: Frame, now: Cycles) {
        self.advance(now);
        if !self.clock_off() {
            self.sci.receive(frame, now);
        }
    }

    /// The frames the SCI sent on TXD since the last call, each as its
    /// start bit goes out, with its start, end and bit time.
    pub fn take_transmitted(&mut self) -> Vec<Transmitted> {
        self.sci.take_transmitted()
    }

    /// The frames sent on TXD that `take_transmitted` has not taken.
    #[must_use]
    pub const fn untaken_frames(&self) -> usize {
        self.sci.untaken_frames()
    }

    /// The board drives port QS's input pins to `pins` at `now`.
    pub fn set_port_pins(&mut self, pins: u8, now: Cycles) {
        self.advance(now);
        self.port.pins = pins;
        if !self.clock_off() {
            let before = self.port_outputs();
            self.pins_changed(now);
            self.record_outputs(before, now);
        }
    }

    /// An external SPI master drives PCS0/SS, the board's pin, low if
    /// `asserted`, else high, at `now`: it selects the QSPI as a slave,
    /// or faults a master (QSMRM 4.4.2, 4.3.5).
    pub fn set_slave_select(&mut self, asserted: bool, now: Cycles) {
        let pin = 1 << port::PCS0_PIN;
        let pins = if asserted {
            self.port.pins & !pin
        } else {
            self.port.pins | pin
        };
        self.set_port_pins(pins, now);
    }

    /// An external SPI master clocks one bit at `now`, `mosi` on MOSI:
    /// the level the QSPI as a slave drives on MISO for it, if it
    /// drives one (QSMRM 4.4.2.1).  The QSPI takes the bit only while
    /// it is a slave the master selects, with SCK and MOSI its inputs
    /// (MC68331UM Table 6-1).
    pub fn clock_slave(&mut self, mosi: bool, now: Cycles) -> Option<bool> {
        self.advance(now);
        if self.clock_off() {
            return None;
        }
        let before = self.port_outputs();
        let miso = self.qspi.clock_slave(mosi, self.port, now);
        self.record_outputs(before, now);
        miso
    }

    /// The levels the board drives on port QS's input pins.
    #[must_use]
    pub const fn port_pins(&self) -> u8 {
        self.port.pins
    }

    /// The pins port QS drives and their levels.
    #[must_use]
    pub const fn port_outputs(&self) -> PortLevels {
        self.port.outputs(self.qspi.code(), self.sci.txd())
    }

    /// The changes in what port QS drives since the last call, each
    /// with its time, in time order: a PCS pulse the QSPI makes within
    /// a quantum, or one the firmware writes, is both its edges.
    pub fn take_output_changes(&mut self) -> Vec<(Cycles, PortLevels)> {
        mem::take(&mut self.output_changes)
    }

    /// Records what port QS drives at `now` as a change if it differs
    /// from `before`; what it drives now.
    fn record_outputs(&mut self, before: PortLevels, now: Cycles) -> PortLevels {
        let outputs = self.port_outputs();
        if outputs != before {
            self.output_changes.push((now, outputs));
        }
        outputs
    }

    /// When the SCI or the QSPI next changes on its own, seen or not:
    /// where what port QS drives may change.
    fn next_change(&self) -> Option<Cycles> {
        [self.sci.next_event(), self.qspi.next_phase_end()]
            .into_iter()
            .flatten()
            .min()
    }

    /// The levels on PCS\[3:0\].
    #[must_use]
    pub const fn chip_selects(&self) -> u8 {
        self.port.chip_selects(self.qspi.code())
    }

    /// QSMCR's IARB.
    #[must_use]
    pub const fn arbitration(&self) -> u8 {
        (self.qsmcr & QSMCR_IARB_MASK) as u8
    }

    /// The port's pins or registers changed at `now`: the QSPI checks
    /// for a mode fault and the peripheral sees PCS.
    fn pins_changed(&mut self, now: Cycles) {
        self.qspi.check_mode_fault(self.port, now);
        self.qspi.notify(self.port, now);
    }

    /// The SCI's and the QSPI's levels, 0 when not requesting.
    fn levels(&self) -> (InterruptLevel, InterruptLevel) {
        let level = |requesting: bool, field: u8| {
            let level = if requesting { field & LEVEL_MASK } else { 0 };
            InterruptLevel::new(level).expect("three bits")
        };
        (
            level(self.sci.requesting(), self.qilr),
            level(self.qspi.requesting(), self.qilr >> ILQSPI_SHIFT),
        )
    }

    /// The register word at the even `offset`, without a read's side
    /// effects.
    fn register_word(&self, offset: u32) -> u16 {
        match offset {
            QSMCR => self.qsmcr,
            // QTEST is for test mode, which the chip never enters
            QTEST => 0,
            QILR_QIVR => u16::from(self.qilr) << 8 | u16::from(self.qivr | INTV0),
            SCCR0 => self.sci.sccr0(),
            SCCR1 => self.sci.sccr1(),
            SCSR => self.sci.status(),
            SCDR => self.sci.data(),
            PORTQS => self.port.levels(self.qspi.code(), self.sci.txd()).into(),
            PQSPAR_DDRQS => u16::from(self.port.assignment) << 8 | u16::from(self.port.direction),
            SPCR0 => self.qspi.spcr0(),
            SPCR1 => self.qspi.spcr1(),
            SPCR2 => self.qspi.spcr2(),
            SPCR3_SPSR => u16::from(self.qspi.spcr3()) << 8 | u16::from(self.qspi.spsr()),
            RAM..RAM_END => {
                let at = Self::ram_index(offset);
                u16::from_be_bytes([self.qspi.ram[at], self.qspi.ram[at + 1]])
            }
            // the reserved words read zero (QSMRM 3)
            _ => 0,
        }
    }

    /// The QSPI RAM index of the word at `offset`, in it.
    const fn ram_index(offset: u32) -> usize {
        (offset - RAM) as usize
    }

    /// Whether `access` reaches the registers, and whether it reaches
    /// a supervisor-only one from user mode, which reads zero and
    /// ignores writes (QSMRM 3.2.1, SUPV).
    fn admit(&self, access: RegisterAccess) -> Option<bool> {
        if access.supervisor {
            return Some(true);
        }
        if self.qsmcr & QSMCR_SUPV != 0 {
            return None;
        }
        Some(access.word() >= SUPERVISOR_ONLY_END)
    }

    /// The register word `access` reaches written with `word`, the
    /// cycle's bytes merged into the old value.
    fn write_word(&mut self, access: RegisterAccess, word: u16) {
        let now = access.now;
        let offset = access.word();
        let [upper, lower] = word.to_be_bytes();
        match offset {
            QSMCR => self.qsmcr = word & QSMCR_WRITABLE,
            QILR_QIVR => {
                self.qilr = upper & QILR_WRITABLE;
                self.qivr = lower & !INTV0;
            }
            SCCR0 => self.sci.write_sccr0(word, now),
            SCCR1 => self.sci.write_sccr1(word, now),
            SCDR if access.lower() => self.sci.write_data(word, now),
            PORTQS => {
                self.port.latch = lower;
                self.pins_changed(now);
            }
            PQSPAR_DDRQS => {
                self.port.write_assignment(upper);
                self.port.direction = lower;
                self.pins_changed(now);
            }
            SPCR0 => {
                self.port.wired_or = qspi::wired_or(word);
                self.qspi.write_spcr0(word, self.port, now);
                self.pins_changed(now);
            }
            SPCR1 => self.qspi.write_spcr1(word, self.port, now),
            SPCR2 => self.qspi.write_spcr2(word, access.lower(), self.port, now),
            SPCR3_SPSR => {
                if access.upper() {
                    self.qspi.write_spcr3(upper, self.port, now);
                }
                if access.lower() {
                    self.qspi.write_spsr(lower);
                }
            }
            RAM..RAM_END => {
                let at = Self::ram_index(offset);
                self.qspi.ram[at..at + 2].copy_from_slice(&[upper, lower]);
            }
            // QTEST and SCSR ignore writes
            _ => {}
        }
    }
}

impl Timed for Qsm {
    fn next_event(&self) -> Option<Cycles> {
        if self.clock_off() {
            return None;
        }
        [self.sci.next_event(), self.qspi.next_event(self.port)]
            .into_iter()
            .flatten()
            .min()
    }

    /// Brings it to `now` change by change, so each change in what
    /// port QS drives is recorded at its own time.
    fn advance(&mut self, now: Cycles) {
        if self.clock_off() {
            return;
        }
        let mut before = self.port_outputs();
        while let Some(at) = self.next_change().filter(|&at| at <= now) {
            self.sci.advance(at);
            self.qspi.advance(self.port, at);
            before = self.record_outputs(before, at);
        }
    }
}

impl crate::module::sealed::Sealed for Qsm {}

impl Module for Qsm {
    fn block(&self) -> Block {
        BLOCK
    }

    /// A read of SCSR arms its flags, and of SCDR's lower byte or word
    /// clears the armed receive flags (QSMRM 5.2.3); a read of SPSR
    /// arms its flags (4.3.5).
    fn read(&mut self, access: RegisterAccess) -> Option<u32> {
        if !self.admit(access)? {
            return Some(0);
        }
        self.advance(access.now);
        let offset = access.word();
        let word = match offset {
            SCSR => self.sci.read_status(),
            SCDR if access.lower() => self.sci.read_data(),
            SPCR3_SPSR if access.lower() => {
                u16::from(self.qspi.spcr3()) << 8 | u16::from(self.qspi.read_spsr())
            }
            _ => self.register_word(offset),
        };
        Some(access.read_value(word))
    }

    fn peek(&self, access: RegisterAccess) -> Peek {
        match self.admit(access) {
            None => Peek::Unacknowledged,
            Some(false) => Peek::Value(0),
            Some(true) => Peek::Value(access.read_value(self.register_word(access.word()))),
        }
    }

    fn write(&mut self, access: RegisterAccess, value: u32) -> bool {
        match self.admit(access) {
            None => false,
            Some(false) => true,
            Some(true) => {
                self.advance(access.now);
                let before = self.port_outputs();
                let word = access.merge(self.register_word(access.word()), value);
                self.write_word(access, word);
                self.record_outputs(before, access.now);
                true
            }
        }
    }

    fn request(&self) -> Option<InterruptRequest> {
        let (sci, qspi) = self.levels();
        let level = sci.max(qspi);
        (level != InterruptLevel::NONE).then_some(InterruptRequest {
            level,
            arbitration: self.arbitration(),
        })
    }

    /// The QSPI's vector, INTV0 set, if it requests `level`, before the
    /// SCI's at the same level (QSMRM 3.2.3, 3.2.4).
    fn acknowledge(&mut self, level: InterruptLevel, _now: Cycles) -> Option<u8> {
        let (sci, qspi) = self.levels();
        if qspi == level {
            Some(self.qivr | INTV0)
        } else if sci == level {
            Some(self.qivr)
        } else {
            None
        }
    }

    /// Every register to its reset value (QSMRM 3; PLAN.md, "QSM and
    /// GPT on reset"); the QSPI RAM, the board's pins, the peripheral,
    /// the frames already sent and the output changes not taken stay.
    /// What was due before `now` happens first, as the GPT's reset
    /// does.  A reset turns on a clock LPSTOP turned off.
    fn reset(&mut self, now: Cycles) {
        self.advance(now);
        let before = self.port_outputs();
        self.low_power = None;
        let pins = self.port.pins;
        self.qsmcr = QSMCR_RESET;
        self.qilr = 0;
        self.qivr = QIVR_RESET & !INTV0;
        self.port = PortQs {
            pins,
            ..PortQs::new()
        };
        self.sci.reset();
        self.qspi.reset(self.port, now);
        self.record_outputs(before, now);
    }

    /// Its registers, the SCI's and the QSPI's (the QSPI RAM and queue
    /// included), the frames on the SCI's lines and the output changes
    /// not taken; not the host's QSPI peripheral.
    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(STATE_VERSION, self))
    }

    /// The host's QSPI peripheral stays.
    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        let saved: Self = decode(STATE_VERSION, snapshot)?;
        if saved.qilr & !QILR_WRITABLE != 0 || saved.qivr & INTV0 != 0 || !saved.port.is_valid() {
            return Err(StateError::invalid("QSM registers"));
        }
        if !saved.sci.is_valid() {
            return Err(StateError::invalid("SCI"));
        }
        if !saved.qspi.is_valid() {
            return Err(StateError::invalid("QSPI"));
        }
        self.qsmcr = saved.qsmcr;
        self.qilr = saved.qilr;
        self.qivr = saved.qivr;
        self.port = saved.port;
        self.sci = saved.sci;
        self.qspi.restore(saved.qspi);
        self.low_power = saved.low_power;
        self.output_changes = saved.output_changes;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
