//! The general-purpose timer (GPTRM; MC68331UM 7, D.2).
//!
//! Its parts: TCNT on the prescaler, three input captures, four output
//! compares and a fifth that is either, the pulse accumulator, the PWM
//! unit, port GP and eleven interrupt sources.  The host drives the
//! input pins and reads the output pins through the chip.

pub mod pwm;
mod timer;

use std::collections::VecDeque;
use std::fmt;
use std::iter;
use std::mem;
use std::str::FromStr;

use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::interrupt::InterruptLevel;
use mc683xx_modules::module::Block;
use mc683xx_modules::module::InterruptRequest;
use mc683xx_modules::module::Module;
use mc683xx_modules::module::Peek;
use mc683xx_modules::module::RegisterAccess;
use mc683xx_modules::peripheral::Timed;
use mc683xx_modules::pins::PortLevels;
use serde::Deserialize;
use serde::Deserializer;
use serde::de::Error;
use serde::de::Unexpected;

use self::pwm::Channel;
use self::pwm::Output;
use self::pwm::Pwm;
use self::timer::Counter;
use self::timer::Prescaler;

/// The GPT's register block, $YFF900 to $YFF93F (MC68331UM Table D-1).
pub const BLOCK: Block = Block {
    offset: 0x900,
    length: 0x40,
};

/// Register offsets in the block (MC68331UM Table D-2; GPTRM Table
/// 1-1), each a word's even address.
const GPTMCR: u32 = 0x00;
/// The test register, reserved: reads zero (MC68331UM D.2.2).
const GPTMTR: u32 = 0x02;
/// ICR.
const ICR: u32 = 0x04;
/// DDRGP in the upper byte, PORTGP in the lower.
const DDRGP_PORTGP: u32 = 0x06;
/// OC1M in the upper byte, OC1D in the lower.
const OC1M_OC1D: u32 = 0x08;
/// TCNT.
const TCNT: u32 = 0x0a;
/// PACTL in the upper byte, PACNT in the lower.
const PACTL_PACNT: u32 = 0x0c;
/// TIC1; TIC2 and TIC3 follow.
const TIC1: u32 = 0x0e;
/// TIC3.
const TIC3: u32 = 0x12;
/// TOC1; TOC2 to TOC4 follow.
const TOC1: u32 = 0x14;
/// TOC4.
const TOC4: u32 = 0x1a;
/// TI4/O5.
const TI4O5: u32 = 0x1c;
/// TCTL1 in the upper byte, TCTL2 in the lower.
const TCTL: u32 = 0x1e;
/// TMSK1 and TMSK2.
const TMSK: u32 = 0x20;
/// TFLG1 and TFLG2.
const TFLG: u32 = 0x22;
/// CFORC in the upper byte, PWMC in the lower.
const CFORC_PWMC: u32 = 0x24;
/// PWMA in the upper byte, PWMB in the lower.
const PWMA_PWMB: u32 = 0x26;
/// PWMCNT.
const PWMCNT: u32 = 0x28;
/// PWMBUFA and PWMBUFB.
const PWMBUF: u32 = 0x2a;
/// PRESCL.
const PRESCL: u32 = 0x2c;
/// The registers that are supervisor-only whatever SUPV (MC68331UM
/// Table D-2, "S").
const SUPERVISOR_ONLY_END: u32 = 0x06;
/// Bytes in a register word.
const WORD_BYTES: u32 = 2;

/// GPTMCR STOP: the clocks stop (GPTRM 9.2).
const GPTMCR_STOP: u16 = 1 << 15;
/// GPTMCR STOPP: the prescaler and pulse accumulator stop, the input
/// pins are ignored (GPTRM 9.4).
const GPTMCR_STOPP: u16 = 1 << 12;
/// GPTMCR INCP: one prescaler count while STOPP; self-negating.
const GPTMCR_INCP: u16 = 1 << 11;
/// GPTMCR SUPV.
const GPTMCR_SUPV: u16 = 1 << 7;
/// GPTMCR IARB.
const GPTMCR_IARB_MASK: u16 = 0xf;
/// GPTMCR's bits that read back: STOP, FRZ1, FRZ0, STOPP, SUPV, IARB.
const GPTMCR_WRITABLE: u16 = 0xf08f;
/// GPTMCR out of reset: SUPV (GPTRM 9.5; MC68331UM D.2.1).
const GPTMCR_RESET: u16 = 0x0080;
/// ICR PAB, bits 15:12: the source moved to the top (GPTRM 7.1.1).
const ICR_PAB_SHIFT: u16 = 12;
/// ICR IRL, bits 10:8.
const ICR_IRL_SHIFT: u16 = 8;
/// ICR IRL, unshifted.
const ICR_IRL_MASK: u16 = 0b111;
/// ICR IVBA, bits 7:4.
const ICR_IVBA_MASK: u16 = 0xf0;
/// ICR's implemented bits.
const ICR_WRITABLE: u16 = 0xf7f0;
/// OC1M's and OC1D's bits, 7:3 for OC5 to OC1 (GPTRM 3.3.3).
const OC1_PINS: u8 = 0xf8;
/// OC1's pin, PGP3; OC2 to OC5 follow (MC68331UM Figure 7-1).
const OC1_PIN: u8 = 3;
/// IC4/OC5's pin, PGP7.
const IC4_PIN: u8 = 7;
/// Input captures 1 to 3 on PGP0 to PGP2.
const CAPTURES: u8 = 3;
/// The format of the GPT's snapshot ([`Module::save`]).
const STATE_VERSION: u32 = 4;
/// Output compares 1 to 4, TOC1 to TOC4; OC5 is TI4/O5.
const COMPARES: usize = 4;
/// PACTL PAIS, the PAI pin (GPTRM 4.1.1).
const PACTL_PAIS: u8 = 1 << 7;
/// PACTL PAEN.
const PACTL_PAEN: u8 = 1 << 6;
/// PACTL PAMOD: gated time accumulation.
const PACTL_PAMOD: u8 = 1 << 5;
/// PACTL PEDGE.
const PACTL_PEDGE: u8 = 1 << 4;
/// PACTL PCLKS, the PCLK pin.
const PACTL_PCLKS: u8 = 1 << 3;
/// PACTL I4/O5: input capture 4, else output compare 5.
const PACTL_I4O5: u8 = 1 << 2;
/// PACTL PACLK, the gated mode's clock (GPTRM Table 4-2).
const PACTL_PACLK_MASK: u8 = 0b11;
/// PACTL's writable bits.
const PACTL_WRITABLE: u8 = 0x77;
/// PACLK: the system clock / 512.
const PACLK_512: u8 = 0b00;
/// PACLK: TCNT's clock.
const PACLK_TCNT: u8 = 0b01;
/// PACLK: TOF.
const PACLK_TOF: u8 = 0b10;
/// The system clock divisor of PACLK %00 (GPTRM Table 4-2).
const PACLK_512_DIVISOR: u64 = 512;
/// TMSK2 CPR, bits 2:0: TCNT's tap, system clock / 2^(CPR + 2), or
/// PCLK (GPTRM Table 5-1).
const TMSK2_CPR_MASK: u8 = 0b111;
/// CPR's PCLK value.
const CPR_PCLK: u8 = 0b111;
/// CPR 0's divisor, 4.
const CPR_LEAST_DIVISOR: u64 = 4;
/// TMSK2 CPROUT, in the word's lower byte: OC1 carries TCNT's clock
/// (GPTRM 7.1.3.1, 5.1).
const TMSK2_CPROUT: u16 = 1 << 3;
/// The time CPROUT's and PPROUT's clocks are high each period: one
/// system clock (GPTRM 5.1).
const CLOCK_OUT_HIGH: Cycles = Cycles(1);
/// TMSK2's implemented bits: TOI, PAOVI, PAII, CPROUT, CPR.
const TMSK2_WRITABLE: u8 = 0xbf;
/// TFLG2 TOF, and TMSK2 TOI, in the word's lower byte (GPTRM 7.1.2.1).
const TOF: u16 = 1 << 7;
/// TFLG2 PAOVF and TMSK2 PAOVI.
const PAOVF: u16 = 1 << 5;
/// TFLG2 PAIF and TMSK2 PAII.
const PAIF: u16 = 1 << 4;
/// TFLG1 IC1F, the word's bit 8; IC2F, IC3F, OC1F to OC4F and I4/O5F
/// follow.
const IC1F: u16 = 1 << 8;
/// TFLG1 OC1F.
const OC1F: u16 = 1 << 11;
/// TFLG1 I4/O5F.
const I4O5F: u16 = 1 << 15;
/// The flags in TFLG1 and TFLG2.
const FLAGS: u16 = 0xff00 | TOF | PAOVF | PAIF;
/// The interrupt sources' flags by source number, 1 (IC1, the
/// highest) to 11 (PAIF) (GPTRM Table 7-1).
const SOURCES: [u16; 11] = [
    IC1F,
    IC1F << 1,
    IC1F << 2,
    OC1F,
    OC1F << 1,
    OC1F << 2,
    OC1F << 3,
    I4O5F,
    TOF,
    PAOVF,
    PAIF,
];
/// CFORC FOC1, bit 11 of the word; FOC2 to FOC5 follow (GPTRM 3.3.4).
const FOC1: u8 = 1 << 3;
/// CFORC FPWMA and FPWMB.
const CFORC_FPWM: u8 = 0b11;
/// PWMC F1A and F1B, which read the pins (GPTRM 6.3.1).
const PWMC_F1: u8 = 0b11;
/// PRESCL's bits (MC68331UM D.2.18).
const PRESCL_MASK: u64 = 0x1ff;
/// The span of TCNT, 16 bits.
const TCNT_SPAN: u64 = 1 << 16;
/// The span of PACNT, 8 bits.
const PACNT_SPAN: u64 = 1 << 8;
/// The capture and compare registers out of reset (GPTRM 3.2.1, 3.3.2).
const CAPTURE_RESET: u16 = 0xffff;

/// TCNT's divisor of the system clock by TMSK2 `tmsk2`'s CPR, or
/// `None` for PCLK (GPTRM 5.1).
const fn cpr_divisor(tmsk2: u8) -> Option<u64> {
    match tmsk2 & TMSK2_CPR_MASK {
        CPR_PCLK => None,
        cpr => Some(CPR_LEAST_DIVISOR << cpr),
    }
}

/// Port GP's pins, PGP0 to PGP7: IC1 to IC3, OC1 to OC4 and IC4/OC5
/// (MC68331UM Figure 7-1).
const PORT_GP_PINS: u8 = 8;

/// One of port GP's pins, by its bit, 0 to 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
pub struct PortGpPin(u8);

/// A snapshot's pin, refused past PGP7.
impl<'de> Deserialize<'de> for PortGpPin {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bit = u8::deserialize(deserializer)?;
        Self::new(bit).ok_or_else(|| {
            D::Error::invalid_value(Unexpected::Unsigned(bit.into()), &"a port GP pin, 0 to 7")
        })
    }
}

impl PortGpPin {
    /// PGP`bit`, if there is one.
    #[must_use]
    pub const fn new(bit: u8) -> Option<Self> {
        if bit < PORT_GP_PINS {
            Some(Self(bit))
        } else {
            None
        }
    }

    /// Its bit.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Its bit in a mask of port GP.
    const fn mask(self) -> u8 {
        1 << self.0
    }
}

/// An input pin of the GPT the host can schedule an edge on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Input {
    /// A pin of port GP: an input capture's, or a general-purpose
    /// input (GPTRM 3.2, 8.1).
    PortGp(PortGpPin),
    /// PAI, the pulse accumulator's input (GPTRM 4).
    Pai,
    /// PCLK, the external clock (GPTRM 2.7).
    Pclk,
}

/// Its pin's name: `PGP0` to `PGP7`, `PAI`, `PCLK` (MC68331UM Figure
/// 7-1).
impl fmt::Display for Input {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PortGp(pin) => write!(formatter, "PGP{}", pin.get()),
            Self::Pai => formatter.write_str("PAI"),
            Self::Pclk => formatter.write_str("PCLK"),
        }
    }
}

/// The input whose pin is named so, as `Display` writes it.
impl FromStr for Input {
    type Err = ParseInputError;

    fn from_str(name: &str) -> Result<Self, ParseInputError> {
        match name {
            "PAI" => Ok(Self::Pai),
            "PCLK" => Ok(Self::Pclk),
            _ => name
                .strip_prefix("PGP")
                .filter(|bit| bit.len() == 1)
                .and_then(|bit| bit.parse().ok())
                .and_then(PortGpPin::new)
                .map(Self::PortGp)
                .ok_or_else(|| ParseInputError(name.to_owned())),
        }
    }
}

/// Text that names no GPT input pin.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no GPT input {0:?}")]
pub struct ParseInputError(pub String);

/// A change in what the GPT drives on its outputs, in the shape of the
/// SIM ports' changes ([PLAN.md], Peripherals).
///
/// [PLAN.md]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum OutputChange {
    /// Port GP's pins: those it drives, the output compares' and
    /// PORTGP's outputs, and their levels.
    PortGp(PortLevels),
    /// A PWM pin's level, while the host asks for its edges.
    Pwm {
        /// The pin.
        channel: Channel,
        /// Its level.
        level: bool,
    },
}

/// How far a fast output's edges have been found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Found {
    /// The prescaler's count then.
    count: u64,
    /// The time.
    time: Cycles,
}

impl Found {
    /// Found up to `time`, the prescaler at `count`.
    const fn new(count: u64, time: Cycles) -> Self {
        Self { count, time }
    }
}

/// The output changes found for the host, which a reset keeps (PLAN.md,
/// Peripherals).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct Edges {
    /// Changes the host has not taken, each with its time, in the order
    /// found.
    queue: Vec<(Cycles, OutputChange)>,
    /// Port GP's driven pins and their levels, as last reported.
    port: PortLevels,
    /// Each PWM pin's level as last reported, while the host asks for
    /// its edges.
    pwm: [Option<bool>; 2],
    /// How far the PWM edges are found.
    pwm_found: Found,
    /// TCNT's clock's level as last found, while the host asks for
    /// CPROUT's edges on OC1.
    oc1_clock: Option<bool>,
    /// How far OC1's clock edges are found.
    oc1_found: Found,
}

/// The levels on the GPT's input pins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Inputs {
    /// PGP7 to PGP0.
    port: u8,
    /// PAI.
    pai: bool,
    /// PCLK.
    pclk: bool,
}

impl Inputs {
    /// Every pin high.
    const HIGH: Self = Self {
        port: u8::MAX,
        pai: true,
        pclk: true,
    };
}

/// An output compare's pin action (GPTRM Table 3-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Disconnected from the pin.
    None,
    /// Toggle.
    Toggle,
    /// Clear to zero.
    Clear,
    /// Set to one.
    Set,
}

/// The GPT's state.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Gpt {
    /// GPTMCR.
    mcr: u16,
    /// Whether LPSTOP has turned its clock off (MC68331UM 4.3.4): it
    /// stops as STOP stops it.
    low_power: bool,
    /// ICR.
    icr: u16,
    /// DDRGP.
    direction: u8,
    /// PORTGP's latch.
    latch: u8,
    /// OC1M.
    oc1_mask: u8,
    /// OC1D.
    oc1_data: u8,
    /// The prescaler.
    prescaler: Prescaler,
    /// TCNT.
    counter: Counter,
    /// The TCNT count whose compares and overflow have been taken.
    tick: u64,
    /// PACTL's writable bits.
    pactl: u8,
    /// PACNT.
    pacnt: u8,
    /// The gated mode clock's count when last taken.
    gated: u64,
    /// Rising edges on PCLK.
    pclk_edges: u64,
    /// TIC1 to TIC3.
    captures: [u16; CAPTURES as usize],
    /// TOC1 to TOC4.
    compares: [u16; COMPARES],
    /// TI4/O5.
    ti4o5: u16,
    /// TCTL1 in the upper byte, TCTL2 in the lower.
    tctl: u16,
    /// TMSK1 and TMSK2.
    tmsk: u16,
    /// Whether CPR has had its one write (GPTRM 5.1).
    cpr_written: bool,
    /// TFLG1 and TFLG2.
    flags: u16,
    /// The flags a read armed to clear (GPTRM 7.1.2).
    armed: u16,
    /// The output compares' levels on the pins they drive.
    compare_levels: u8,
    /// The PWM unit.
    pwm: Pwm,
    /// The levels the board drives on the input pins.
    inputs: Inputs,
    /// The input levels the GPT has seen: frozen while STOP or STOPP.
    seen: Inputs,
    /// Port GP's pin levels as last seen, for input capture edges.
    levels: u8,
    /// When the GPT last saw PCLK rise, the start of the clock outputs'
    /// pulse when PCLK clocks their counter.
    pclk_rise: Option<Cycles>,
    /// The output edges for the host.
    edges: Edges,
    /// The input edges the board makes later: when, which pin and its
    /// level, in time order, those at one time in the order given.
    scheduled: VecDeque<(Cycles, Input, bool)>,
    /// The time it has been brought to.
    time: Cycles,
    /// `next_event`, found when it may have moved; not saved, but found
    /// again on restore.
    #[serde(skip)]
    next: Option<Cycles>,
}

impl Default for Gpt {
    fn default() -> Self {
        Self::new(Cycles::ZERO)
    }
}

impl Gpt {
    /// The GPT out of reset at `now`, the board's pins high.
    #[must_use]
    pub const fn new(now: Cycles) -> Self {
        Self::with_inputs(now, Inputs::HIGH)
    }

    /// The GPT out of reset at `now`, the board driving `inputs`.
    const fn with_inputs(now: Cycles, inputs: Inputs) -> Self {
        Self {
            mcr: GPTMCR_RESET,
            low_power: false,
            icr: 0,
            direction: 0,
            latch: 0,
            oc1_mask: 0,
            oc1_data: 0,
            prescaler: Prescaler::new(now),
            counter: Counter::new(Some(CPR_LEAST_DIVISOR), 0),
            tick: 0,
            pactl: 0,
            pacnt: 0,
            gated: 0,
            pclk_edges: 0,
            captures: [CAPTURE_RESET; CAPTURES as usize],
            compares: [CAPTURE_RESET; COMPARES],
            ti4o5: CAPTURE_RESET,
            tctl: 0,
            tmsk: 0,
            cpr_written: false,
            flags: 0,
            armed: 0,
            compare_levels: 0,
            pwm: Pwm::new(0),
            inputs,
            seen: inputs,
            levels: inputs.port,
            pclk_rise: None,
            edges: Edges {
                queue: Vec::new(),
                port: PortLevels {
                    driven: 0,
                    levels: 0,
                },
                pwm: [None; 2],
                pwm_found: Found::new(0, now),
                oc1_clock: None,
                oc1_found: Found::new(0, now),
            },
            scheduled: VecDeque::new(),
            time: now,
            next: None,
        }
    }

    /// GPTMCR's IARB.
    #[must_use]
    pub const fn arbitration(&self) -> u8 {
        (self.mcr & GPTMCR_IARB_MASK) as u8
    }

    /// The board drives port GP's input pins to `pins` at `now`.
    pub fn set_port_pins(&mut self, pins: u8, now: Cycles) {
        self.advance(now);
        self.inputs.port = pins;
        self.input_changed(now);
        self.refresh();
    }

    /// The board drives PAI to `level` at `now`.
    pub fn set_pai(&mut self, level: bool, now: Cycles) {
        self.advance(now);
        self.drive(Input::Pai, level, now);
        self.refresh();
    }

    /// The board drives PCLK to `level` at `now`.
    pub fn set_pclk(&mut self, level: bool, now: Cycles) {
        self.advance(now);
        self.drive(Input::Pclk, level, now);
        self.refresh();
    }

    /// The board will drive `input` to `level` at `at`, or at once if
    /// that is past: the GPT captures, counts and clocks at that time,
    /// and `next_event` ends a quantum there.  A reset keeps it.
    pub fn schedule_input(&mut self, at: Cycles, input: Input, level: bool) {
        let at = at.max(self.time);
        let index = self.scheduled.partition_point(|&(when, _, _)| when <= at);
        self.scheduled.insert(index, (at, input, level));
        self.refresh();
    }

    /// Withdraws the inputs `schedule_input` scheduled past `now`, those
    /// up to it taking effect first: for a board whose clocks moved
    /// against its own time, to schedule them again at their new clocks.
    pub fn unschedule_inputs(&mut self, now: Cycles) {
        self.advance(now);
        self.scheduled.clear();
        self.refresh();
    }

    /// The board drives `input` to `level` at `now`.
    fn drive(&mut self, input: Input, level: bool, now: Cycles) {
        match input {
            Input::PortGp(pin) => {
                self.inputs.port =
                    self.inputs.port & !pin.mask() | if level { pin.mask() } else { 0 };
            }
            Input::Pai => self.inputs.pai = level,
            Input::Pclk => self.inputs.pclk = level,
        }
        self.input_changed(now);
    }

    /// The pins of port GP the GPT drives and their levels at `now`:
    /// the output compares' on the pins they have, TCNT's clock on OC1's
    /// under CPROUT, PORTGP's on other outputs (GPTRM 3.3.1, 8.1, Table
    /// B-1).
    #[must_use]
    pub const fn port_outputs(&self, now: Cycles) -> PortLevels {
        self.port_outputs_with(self.compare_clock_out() && self.clock_high(self.counter, now))
    }

    /// The pins of port GP the GPT drives and their levels, TCNT's clock
    /// on OC1's pin at `clock` under CPROUT.
    const fn port_outputs_with(&self, clock: bool) -> PortLevels {
        let compared = self.compared_pins();
        let clocked = if self.compare_clock_out() {
            1 << OC1_PIN
        } else {
            0
        };
        let driven = self.direction | compared | clocked;
        let clock_level = if clock { clocked } else { 0 };
        PortLevels {
            driven,
            levels: self.latch & self.direction & !compared & !clocked
                | self.compare_levels & compared
                | clock_level,
        }
    }

    /// Whether OC1's pin carries TCNT's clock: CPROUT, with OC1M3 clear
    /// (GPTRM Table B-1).
    const fn compare_clock_out(&self) -> bool {
        self.tmsk & TMSK2_CPROUT != 0 && self.oc1_mask & 1 << OC1_PIN == 0
    }

    /// Whether the clock of `counter`, TCNT or PWMCNT, is high at `now`,
    /// as CPROUT and PPROUT drive it out (GPTRM 5.1): on PCLK, the one
    /// system clock from each rising edge the GPT sees.
    const fn clock_high(&self, counter: Counter, now: Cycles) -> bool {
        match counter.clock_high(self.prescaler.count(now)) {
            Some(high) => high,
            None => match self.pclk_rise {
                Some(rise) => now.0 < rise.0 + CLOCK_OUT_HIGH.0,
                None => false,
            },
        }
    }

    /// The edges of the clock of `counter` after `from` up to `now`,
    /// each at its time with its level: on PCLK, only the falls, the
    /// rises being found as PCLK rises.
    fn clock_edges(
        &self,
        counter: Counter,
        from: Found,
        now: Cycles,
    ) -> impl Iterator<Item = (Cycles, bool)> + use<> {
        let prescaler = self.prescaler;
        let to = prescaler.count(now);
        let fall = self
            .pclk_rise
            .map(|rise| rise + CLOCK_OUT_HIGH)
            .filter(|&fall| counter.on_pclk() && from.time < fall && fall <= now)
            .map(|fall| (fall, false));
        iter::successors(counter.next_clock_edge(from.count), move |&count| {
            counter.next_clock_edge(count)
        })
        .take_while(move |&count| count <= to)
        .map(move |count| {
            let at = prescaler.when(count).map_or(now, |at| at.min(now));
            (at, counter.clock_high(count) == Some(true))
        })
        .chain(fall)
    }

    /// The output changes since the last call, each at the time it
    /// happened, in time order ([PLAN.md], Peripherals).
    ///
    /// [PLAN.md]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md
    pub fn take_output_changes(&mut self) -> Vec<(Cycles, OutputChange)> {
        let mut changes = mem::take(&mut self.edges.queue);
        changes.sort_by_key(|&(at, _)| at);
        changes
    }

    /// Port GP's driven pins and their levels, and the PWM pins' levels
    /// (bit 0 PWMA, bit 1 PWMB, while their edges are reported), as the
    /// output changes last reported them.
    pub(crate) fn reported_outputs(&self) -> (PortLevels, u8) {
        let pwm = Channel::BOTH
            .into_iter()
            .filter(|channel| self.edges.pwm[channel.index()] == Some(true))
            .fold(0, |bits, channel| bits | 1 << channel.index());
        (self.edges.port, pwm)
    }

    /// Whether `channel`'s PWM edges are reported from `now`, from the
    /// level its pin has then, PPROUT's clock's included.  Off out of
    /// power-on.
    pub fn set_report_pwm_edges(&mut self, channel: Channel, report: bool, now: Cycles) {
        self.advance(now);
        self.edges.pwm[channel.index()] = report.then(|| self.pwm_level(channel, now));
    }

    /// Whether the edges of TCNT's clock on OC1's pin under CPROUT are
    /// reported from `now`, in port GP's changes; while they are not,
    /// the changes show the pin low.  Off out of power-on.
    pub fn set_report_oc1_clock_edges(&mut self, report: bool, now: Cycles) {
        self.advance(now);
        self.edges.oc1_clock = report.then(|| self.clock_high(self.counter, now));
        self.edges.oc1_found = Found::new(self.prescaler.count(now), now);
        self.report_port(now);
    }

    /// What `channel`'s PWM pin carries at `now`.
    #[must_use]
    pub const fn pwm(&self, channel: Channel, now: Cycles) -> Output {
        self.pwm.output(channel, self.prescaler.count(now))
    }

    /// What OC1's pin carries at `now`: TCNT's clock under CPROUT with
    /// OC1M3 clear (GPTRM 5.1, Table B-1), otherwise the level PORTGP
    /// reads on it.
    #[must_use]
    pub const fn oc1(&self, now: Cycles) -> Output {
        if self.compare_clock_out() {
            Output::Clock {
                clocks: self.counter.divisor(),
            }
        } else {
            Output::Level(self.port_levels(now) & 1 << OC1_PIN != 0)
        }
    }

    /// The level on `channel`'s PWM pin at `now`: PWMCNT's clock under
    /// PPROUT (GPTRM 6.3.1).
    #[must_use]
    pub const fn pwm_level(&self, channel: Channel, now: Cycles) -> bool {
        if self.pwm.clock_out(channel) {
            self.clock_high(self.pwm.counter(), now)
        } else {
            self.pwm.level(channel, self.prescaler.count(now))
        }
    }

    /// Port GP's pins at `now` as PORTGP reads them: the driven level on
    /// outputs, the board's on inputs (GPTRM 8.1).
    const fn port_levels(&self, now: Cycles) -> u8 {
        self.port_outputs(now).pin_levels(self.seen.port)
    }

    /// Whether IC4/OC5 is input capture 4.
    const fn input_capture_4(&self) -> bool {
        self.pactl & PACTL_I4O5 != 0
    }

    /// Output compare `number`'s pin action, 2 to 5, from TCTL1.
    const fn action(&self, number: u8) -> Action {
        if number == 5 && self.input_capture_4() {
            return Action::None;
        }
        match self.tctl >> (8 + 2 * (number - 2)) & 0b11 {
            0b00 => Action::None,
            0b01 => Action::Toggle,
            0b10 => Action::Clear,
            _ => Action::Set,
        }
    }

    /// The pins the output compares drive: OC2 to OC5's with an action,
    /// and OC1M's (GPTRM 3.3.1, 3.3.3).
    const fn compared_pins(&self) -> u8 {
        let mut pins = self.oc1_mask & OC1_PINS;
        let mut number = 2;
        while number <= 5 {
            if !matches!(self.action(number), Action::None) {
                pins |= 1 << (OC1_PIN + number - 1);
            }
            number += 1;
        }
        pins
    }

    /// Whether STOP, STOPP or LPSTOP stops the prescaler and the input
    /// pins.
    const fn stopped(&self) -> bool {
        self.mcr & (GPTMCR_STOP | GPTMCR_STOPP) != 0 || self.low_power
    }

    /// LPSTOP turns its clock off at `now`, or a wake turns it on: TCNT,
    /// the prescaler and the inputs stop as STOP stops them, and run on
    /// from where they stopped (MC68331UM 4.3.4; PLAN.md, LPSTOP).
    pub(crate) fn set_low_power(&mut self, low_power: bool, now: Cycles) {
        self.advance(now);
        let was = self.stopped();
        self.low_power = low_power;
        self.stop_changed(was, now);
        self.find_fast_edges(now);
        self.refresh();
    }

    /// The board changed an input at `now`: the GPT sees it unless
    /// stopped.
    fn input_changed(&mut self, now: Cycles) {
        self.catch_up(now);
        if !self.stopped() {
            self.synchronize(now);
            self.find_fast_edges(now);
        }
    }

    /// The input synchronizers take the board's levels at `now`: PCLK's
    /// rising edges clock, PAI's edges count, port GP's capture.
    fn synchronize(&mut self, now: Cycles) {
        let before = self.seen;
        self.seen = self.inputs;
        if self.seen.pclk && !before.pclk {
            self.pclk_rise = Some(now);
            self.pclk_edges += 1;
            self.counter.pclk();
            self.pwm.pclk();
            self.catch_up(now);
        }
        if self.seen.pai != before.pai {
            self.pai_edge(before.pai, now);
        }
        self.pins_changed(now);
    }

    /// PAI changed from `was` at `now` (GPTRM 4.1): an event counted,
    /// or the end of a gated period.
    fn pai_edge(&mut self, was: bool, now: Cycles) {
        if self.pactl & PACTL_PAEN == 0 {
            return;
        }
        let rising = self.seen.pai && !was;
        let pedge = self.pactl & PACTL_PEDGE != 0;
        if self.pactl & PACTL_PAMOD == 0 {
            if rising == pedge {
                self.accumulate(1);
                self.flags |= PAIF;
            }
        } else if self.gate_open_at(was) && !self.gate_open() {
            self.flags |= PAIF;
        }
        self.gated = self.gated_count(self.prescaler.count(now));
    }

    /// Whether PAI at `level` lets the gated mode count (GPTRM Table
    /// 4-1): PEDGE clear, a zero inhibits; set, a one.
    const fn gate_open_at(&self, level: bool) -> bool {
        level != (self.pactl & PACTL_PEDGE != 0)
    }

    /// Whether the gated mode counts now.
    const fn gate_open(&self) -> bool {
        self.pactl & (PACTL_PAEN | PACTL_PAMOD) == PACTL_PAEN | PACTL_PAMOD
            && self.gate_open_at(self.seen.pai)
    }

    /// The gated mode clock's count at the prescaler's `count` (GPTRM
    /// Table 4-2).
    const fn gated_count(&self, count: u64) -> u64 {
        match self.pactl & PACTL_PACLK_MASK {
            PACLK_512 => count / PACLK_512_DIVISOR,
            PACLK_TCNT => self.counter.ticks(count),
            PACLK_TOF => self.counter.ticks(count) / TCNT_SPAN,
            _ => self.pclk_edges,
        }
    }

    /// PACNT counts `events`: PAOVF as it passes $FF (GPTRM 4.1).
    fn accumulate(&mut self, events: u64) {
        let total = u64::from(self.pacnt) + events;
        if total >= PACNT_SPAN {
            self.flags |= PAOVF;
        }
        self.pacnt = (total % PACNT_SPAN) as u8;
    }

    /// What port GP drives may have changed at `at`: a change if it did,
    /// OC1's clock at its level as last found.
    fn report_port(&mut self, at: Cycles) {
        let outputs = self.port_outputs_with(self.edges.oc1_clock == Some(true));
        if outputs != self.edges.port {
            self.edges.port = outputs;
            self.edges.queue.push((at, OutputChange::PortGp(outputs)));
        }
    }

    /// The reported fast outputs' edges up to `now`: the PWM pins' and
    /// OC1's clock's (PLAN.md, Peripherals).
    fn find_fast_edges(&mut self, now: Cycles) {
        self.find_pwm_edges(now);
        self.find_oc1_clock_edges(now);
    }

    /// The reported PWM pins' edges up to `now`: those the PWM unit's
    /// schedule or PWMA's clock under PPROUT makes, each at its own
    /// time, then a level a change of registers or a PCLK edge made now
    /// (PLAN.md, Peripherals).
    fn find_pwm_edges(&mut self, now: Cycles) {
        let count = self.prescaler.count(now);
        let from = self.edges.pwm_found;
        for channel in Channel::BOTH {
            if self.edges.pwm[channel.index()].is_none() {
                continue;
            }
            if self.pwm.clock_out(channel) {
                for (at, level) in self.clock_edges(self.pwm.counter(), from, now) {
                    self.report_pwm_level(channel, level, at);
                }
            } else {
                let target = self.pwm.count(count);
                let mut ticks = self.pwm.count(from.count);
                while let Some(edge) = self.pwm.next_edge(channel, ticks) {
                    if edge > target {
                        break;
                    }
                    ticks = edge;
                    let at = self
                        .pwm
                        .count_at(edge)
                        .and_then(|edge| self.prescaler.when(edge))
                        .map_or(now, |at| at.min(now));
                    self.report_pwm_level(channel, self.pwm.level_at(channel, edge), at);
                }
            }
            self.report_pwm_level(channel, self.pwm_level(channel, now), now);
        }
        self.edges.pwm_found = Found::new(count, now);
    }

    /// The edges of TCNT's clock on OC1's pin up to `now`, while the
    /// host asks for them, each a port GP change at its own time, then
    /// what a change of registers made now.
    fn find_oc1_clock_edges(&mut self, now: Cycles) {
        if self.edges.oc1_clock.is_some() {
            if self.compare_clock_out() {
                for (at, level) in self.clock_edges(self.counter, self.edges.oc1_found, now) {
                    self.edges.oc1_clock = Some(level);
                    self.report_port(at);
                }
            }
            self.edges.oc1_clock = Some(self.clock_high(self.counter, now));
        }
        self.edges.oc1_found = Found::new(self.prescaler.count(now), now);
        self.report_port(now);
    }

    /// `channel`'s pin is at `level` from `at`: an edge if it changed.
    fn report_pwm_level(&mut self, channel: Channel, level: bool, at: Cycles) {
        let reported = &mut self.edges.pwm[channel.index()];
        if *reported != Some(level) {
            *reported = Some(level);
            let change = OutputChange::Pwm { channel, level };
            self.edges.queue.push((at, change));
        }
    }

    /// Port GP's levels may have changed at `now`: an edge for the host
    /// on an output, and a selected edge on an input capture's pin
    /// latches TCNT and sets its flag, even with the flag set (GPTRM
    /// 3.2).
    fn pins_changed(&mut self, now: Cycles) {
        self.report_port(now);
        let levels = self.port_levels(now);
        let changed = levels ^ self.levels;
        self.levels = levels;
        let tcnt = self.counter.ticks(self.prescaler.count(now)) as u16;
        let channels = (0..CAPTURES).chain(self.input_capture_4().then_some(IC4_PIN));
        for (number, pin) in channels.enumerate() {
            let bit = 1 << pin;
            let edge = (self.tctl >> (2 * number)) & 0b11;
            let rising = levels & bit != 0;
            let captured = changed & bit != 0
                && match edge {
                    0b01 => rising,
                    0b10 => !rising,
                    0b11 => true,
                    _ => false,
                };
            if captured {
                if pin == IC4_PIN {
                    self.ti4o5 = tcnt;
                    self.flags |= I4O5F;
                } else {
                    self.captures[number] = tcnt;
                    self.flags |= IC1F << pin;
                }
            }
        }
    }

    /// The output compares that match TCNT at `tick`, by number: bit
    /// `n - 1` for output compare `n`.
    fn matches(&self, tick: u64) -> u8 {
        let value = tick as u16;
        let mut matched = 0;
        for (index, &compare) in self.compares.iter().enumerate() {
            if compare == value {
                matched |= 1 << index;
            }
        }
        if !self.input_capture_4() && self.ti4o5 == value {
            matched |= 1 << COMPARES;
        }
        matched
    }

    /// The pin actions of the compares in `matched`, by number: OC2 to
    /// OC5's by TCTL1, then OC1's by OC1M and OC1D, which wins a pin
    /// both drive (GPTRM 3.3.3).
    fn act(&mut self, matched: u8) {
        for number in 2..=5 {
            if matched & 1 << (number - 1) == 0 {
                continue;
            }
            let pin = 1 << (OC1_PIN + number - 1);
            match self.action(number) {
                Action::None => {}
                Action::Toggle => self.compare_levels ^= pin,
                Action::Clear => self.compare_levels &= !pin,
                Action::Set => self.compare_levels |= pin,
            }
        }
        if matched & 1 != 0 {
            let mask = self.oc1_mask & OC1_PINS;
            self.compare_levels = self.compare_levels & !mask | self.oc1_data & mask;
        }
    }

    /// The next TCNT count after `tick` at which a compare matches or
    /// TCNT overflows.
    fn next_tick(&self, tick: u64) -> u64 {
        let after = |value: u16| tick + 1 + (u64::from(value).wrapping_sub(tick + 1) % TCNT_SPAN);
        let overflow = (tick / TCNT_SPAN + 1) * TCNT_SPAN;
        let oc5 = (!self.input_capture_4()).then_some(self.ti4o5);
        self.compares
            .iter()
            .copied()
            .chain(oc5)
            .map(after)
            .fold(overflow, u64::min)
    }

    /// When TCNT reaches `tick`, if the prescaler clocks it.
    fn time_of(&self, tick: u64) -> Option<Cycles> {
        self.prescaler.when(self.counter.count_at(tick)?)
    }

    /// Whether the compare and overflow events at the next tick would
    /// show: an interrupt enabled, or a pin action.
    fn tick_matters(&self, tick: u64) -> bool {
        let matched = self.matches(tick);
        let enabled = (self.tmsk >> 8) as u8;
        let compare_flags = (enabled >> OC1_PIN) & 0b1_1111;
        let overflow = tick.is_multiple_of(TCNT_SPAN) && self.tmsk & TOF != 0;
        let pins = (2..=5).any(|number| {
            matched & 1 << (number - 1) != 0 && !matches!(self.action(number), Action::None)
        }) || matched & 1 != 0 && self.oc1_mask & OC1_PINS != 0;
        matched & compare_flags != 0 || overflow || pins
    }

    /// When the gated pulse accumulator next overflows with PAOVI set,
    /// if its clock is the prescaler's.
    fn overflow_time(&self) -> Option<Cycles> {
        if !self.gate_open() || self.tmsk & PAOVF == 0 {
            return None;
        }
        let target = self.gated + PACNT_SPAN - u64::from(self.pacnt);
        match self.pactl & PACTL_PACLK_MASK {
            PACLK_512 => self.prescaler.when(target * PACLK_512_DIVISOR),
            PACLK_TCNT => self.time_of(target),
            PACLK_TOF => self.time_of(target * TCNT_SPAN),
            _ => None,
        }
    }

    /// The source whose vector an acknowledge returns, by number: PAB's
    /// if it is pending, else the lowest pending (GPTRM 7.1, Table 7-1).
    fn pending_source(&self) -> Option<u8> {
        let pending = self.flags & self.tmsk & FLAGS;
        let adjusted = (self.icr >> ICR_PAB_SHIFT) as u8;
        let number = |index: usize| index as u8 + 1;
        SOURCES
            .iter()
            .enumerate()
            .find(|&(index, &flag)| number(index) == adjusted && pending & flag != 0)
            .or_else(|| {
                SOURCES
                    .iter()
                    .enumerate()
                    .find(|&(_, &flag)| pending & flag != 0)
            })
            .map(|(index, _)| number(index))
    }

    /// ICR's IRL.
    const fn level(&self) -> u8 {
        (self.icr >> ICR_IRL_SHIFT & ICR_IRL_MASK) as u8
    }

    /// Whether `access` reaches the registers, and whether it reaches
    /// a supervisor-only one from user mode, which reads zero and
    /// ignores writes (GPTRM 9.5).
    fn admit(&self, access: RegisterAccess) -> Option<bool> {
        if access.supervisor {
            return Some(true);
        }
        if self.mcr & GPTMCR_SUPV != 0 {
            return None;
        }
        Some(access.word() >= SUPERVISOR_ONLY_END)
    }

    /// The register word at the even `offset` at `now`, without a
    /// read's side effects.
    fn register_word(&self, offset: u32, now: Cycles) -> u16 {
        let count = self.prescaler.count(now);
        let pins = |high, low| u16::from_be_bytes([high, low]);
        match offset {
            GPTMCR => self.mcr,
            ICR => self.icr,
            DDRGP_PORTGP => pins(self.direction, self.port_levels(now)),
            OC1M_OC1D => pins(self.oc1_mask, self.oc1_data),
            TCNT => self.counter.ticks(count) as u16,
            PACTL_PACNT => {
                let pais = if self.seen.pai { PACTL_PAIS } else { 0 };
                let pclks = if self.seen.pclk { PACTL_PCLKS } else { 0 };
                pins(self.pactl | pais | pclks, self.pacnt)
            }
            TIC1..=TIC3 => self.captures[((offset - TIC1) / WORD_BYTES) as usize],
            TOC1..=TOC4 => self.compares[((offset - TOC1) / WORD_BYTES) as usize],
            TI4O5 => self.ti4o5,
            TCTL => self.tctl,
            TMSK => self.tmsk,
            TFLG => self.flags,
            CFORC_PWMC => {
                // F1A and F1B read the pins (GPTRM Tables B-5, B-6)
                let f1 = u8::from(self.pwm_level(Channel::A, now)) << 1
                    | u8::from(self.pwm_level(Channel::B, now));
                pins(self.pwm.force, self.pwm.control & !PWMC_F1 | f1)
            }
            PWMA_PWMB => pins(self.pwm.duty[0], self.pwm.duty[1]),
            PWMCNT => self.pwm.count(count) as u16,
            PWMBUF => pins(self.pwm.buffer[0], self.pwm.buffer[1]),
            PRESCL => (count & PRESCL_MASK) as u16,
            // the test register reads zero (MC68331UM D.2.2), as do the
            // unused words
            GPTMTR => 0,
            _ => 0,
        }
    }

    /// GPTMCR written at `now`: STOP and STOPP stop the prescaler and
    /// freeze the inputs; INCP steps it and clocks the synchronizers
    /// once (GPTRM 9.4).
    fn write_mcr(&mut self, value: u16, now: Cycles) {
        let was = self.stopped();
        self.mcr = value & GPTMCR_WRITABLE;
        if was && self.stopped() && value & GPTMCR_INCP != 0 && self.mcr & GPTMCR_STOPP != 0 {
            self.prescaler.step();
            self.synchronize(now);
            self.catch_up(now);
        }
        self.stop_changed(was, now);
    }

    /// Whether the prescaler and the inputs are stopped changed at
    /// `now` from `was`: the prescaler stops or starts, and on a start
    /// the synchronizers take the board's levels.
    fn stop_changed(&mut self, was: bool, now: Cycles) {
        match (was, self.stopped()) {
            (false, true) => self.prescaler.stop(now),
            (true, false) => {
                self.prescaler.start(now);
                self.synchronize(now);
            }
            _ => {}
        }
    }

    /// The forced compares in CFORC's FOC bits take their pin actions
    /// at once, their flags untouched (GPTRM 3.3.4).
    fn force(&mut self, foc: u8, now: Cycles) {
        let matched = foc >> FOC1.trailing_zeros();
        if matched != 0 {
            self.act(matched);
            self.pins_changed(now);
        }
    }

    /// The word at `access`'s register written with `word`, the
    /// cycle's bytes merged into the old value, at the prescaler's
    /// `count`.
    fn write_word(&mut self, access: RegisterAccess, word: u16, count: u64) {
        let now = access.now;
        let offset = access.word();
        let [upper, lower] = word.to_be_bytes();
        match offset {
            GPTMCR => self.write_mcr(word, now),
            ICR => self.icr = word & ICR_WRITABLE,
            // PORTGP reads the pins, not the latch (GPTRM 8.1): only the
            // cycle's lanes change
            DDRGP_PORTGP => {
                if access.upper() {
                    self.direction = upper;
                }
                if access.lower() {
                    self.latch = lower;
                }
                self.pins_changed(now);
            }
            OC1M_OC1D => {
                self.oc1_mask = upper & OC1_PINS;
                self.oc1_data = lower & OC1_PINS;
                self.pins_changed(now);
            }
            PACTL_PACNT => {
                self.pactl = upper & PACTL_WRITABLE;
                self.pacnt = lower;
                self.gated = self.gated_count(count);
                self.pins_changed(now);
            }
            TOC1..=TOC4 => self.compares[((offset - TOC1) / WORD_BYTES) as usize] = word,
            // TI4/O5 is TIC4 with I4/O5 set, read-only (GPTRM 3.4)
            TI4O5 if !self.input_capture_4() => self.ti4o5 = word,
            TCTL => {
                self.tctl = word;
                self.pins_changed(now);
            }
            TMSK => {
                let [_, old] = self.tmsk.to_be_bytes();
                let mut tmsk2 = lower & TMSK2_WRITABLE;
                if access.lower() && !self.cpr_written {
                    // CPR is written once after reset (GPTRM 5.1)
                    self.cpr_written = true;
                    self.counter.set_divisor(cpr_divisor(tmsk2), count);
                } else {
                    tmsk2 = tmsk2 & !TMSK2_CPR_MASK | old & TMSK2_CPR_MASK;
                }
                self.tmsk = u16::from_be_bytes([upper, tmsk2]);
            }
            TFLG => {
                let cleared = self.armed & !word & FLAGS;
                self.flags &= !cleared;
                self.armed &= !cleared;
            }
            CFORC_PWMC => {
                if access.upper() {
                    self.pwm.force = upper & CFORC_FPWM;
                    self.force(upper, now);
                }
                if access.lower() {
                    self.pwm.write_control(lower, count);
                }
            }
            PWMA_PWMB => self.pwm.duty = [upper, lower],
            // TCNT, the captures, PWMCNT, the buffers and PRESCL are
            // read-only outside test and freeze mode (GPTRM 9.1, 9.3)
            _ => {}
        }
    }
}

impl Timed for Gpt {
    fn next_event(&self) -> Option<Cycles> {
        debug_assert_eq!(self.next, self.find_next_event(), "a stale next event");
        self.next
    }

    /// Brings it to `now`: each scheduled input edge at its own time.
    /// The next event stays unless `now` reaches it: until then TCNT
    /// passes only compares that do not show, and the gated
    /// accumulator's overflow count holds.
    fn advance(&mut self, now: Cycles) {
        while let Some(&(at, input, level)) = self.scheduled.front()
            && at <= now
        {
            self.scheduled.pop_front();
            self.catch_up(at);
            self.drive(input, level, at);
        }
        self.catch_up(now);
        if self.next.is_some_and(|next| next <= now) {
            self.refresh();
        }
    }
}

impl Gpt {
    /// Whether a snapshot's GPT is one a run could leave: ICR's
    /// implemented bits only, TCNT and PWMCNT on the taps CPR and PPR
    /// select (GPTRM 5.1), the scheduled inputs in time order.
    fn validate(&self) -> Result<(), StateError> {
        let [_, tmsk2] = self.tmsk.to_be_bytes();
        if self.icr & !ICR_WRITABLE != 0 {
            return Err(StateError::invalid("GPT registers"));
        }
        if self.counter.divisor() != cpr_divisor(tmsk2) || !self.pwm.is_valid() {
            return Err(StateError::invalid("GPT prescaler taps"));
        }
        if !self
            .scheduled
            .iter()
            .zip(self.scheduled.iter().skip(1))
            .all(|(earlier, later)| earlier.0 <= later.0)
        {
            return Err(StateError::invalid("GPT scheduled inputs"));
        }
        Ok(())
    }

    /// The next event, found anew after a change that may move it: a
    /// write, an input, a reset, or reaching the event.
    fn refresh(&mut self) {
        self.next = self.find_next_event();
    }

    /// When it next changes on its own: a compare or overflow that
    /// shows, the gated accumulator's overflow, or a scheduled input.
    fn find_next_event(&self) -> Option<Cycles> {
        let mut tick = self.tick;
        // a compare or overflow that shows comes within a TCNT period,
        // six events at most
        let mut compare = None;
        for _ in 0..=COMPARES + 1 {
            tick = self.next_tick(tick);
            if self.tick_matters(tick) {
                compare = self.time_of(tick);
                break;
            }
        }
        let input = self.scheduled.front().map(|&(at, _, _)| at);
        compare
            .into_iter()
            .chain(self.overflow_time())
            .chain(input)
            .min()
    }

    /// Brings it to `now` with no input edge: each compare match and
    /// overflow in order, at its own count, then the gated accumulator
    /// and the PWM buffers.
    fn catch_up(&mut self, now: Cycles) {
        self.time = self.time.max(now);
        let count = self.prescaler.count(now);
        let target = self.counter.ticks(count);
        loop {
            let tick = self.next_tick(self.tick);
            if tick > target {
                break;
            }
            let at = self.time_of(tick).map_or(now, |at| at.min(now));
            self.find_oc1_clock_edges(at);
            self.tick = tick;
            let matched = self.matches(tick);
            self.flags |= u16::from(matched & 0b1111) << 11;
            if matched & 1 << COMPARES != 0 {
                self.flags |= I4O5F;
            }
            if tick.is_multiple_of(TCNT_SPAN) {
                self.flags |= TOF;
            }
            self.act(matched);
            self.pins_changed(at);
        }
        self.tick = target;
        let gated = self.gated_count(count);
        if self.gate_open() {
            self.accumulate(gated - self.gated);
        }
        self.gated = gated;
        self.find_fast_edges(now);
        self.pwm.sync(count);
    }
}

impl mc683xx_modules::module::sealed::Sealed for Gpt {}

impl Module for Gpt {
    fn block(&self) -> Block {
        BLOCK
    }

    /// A read of TFLG1 or TFLG2 arms its set flags to clear (GPTRM
    /// 7.1.2).
    fn read(&mut self, access: RegisterAccess) -> Option<u32> {
        if !self.admit(access)? {
            return Some(0);
        }
        self.advance(access.now);
        let offset = access.word();
        let word = self.register_word(offset, access.now);
        if offset == TFLG {
            let lanes = access.merge(0, u32::MAX);
            self.armed |= word & lanes;
        }
        Some(access.read_value(word))
    }

    fn peek(&self, access: RegisterAccess) -> Peek {
        match self.admit(access) {
            None => Peek::Unacknowledged,
            Some(false) => Peek::Value(0),
            Some(true) => {
                let word = self.register_word(access.word(), access.now);
                Peek::Value(access.read_value(word))
            }
        }
    }

    fn write(&mut self, access: RegisterAccess, value: u32) -> bool {
        match self.admit(access) {
            None => false,
            Some(false) => true,
            Some(true) => {
                self.advance(access.now);
                let count = self.prescaler.count(access.now);
                let word = access.merge(self.register_word(access.word(), access.now), value);
                self.write_word(access, word, count);
                self.find_fast_edges(access.now);
                self.refresh();
                true
            }
        }
    }

    fn request(&self) -> Option<InterruptRequest> {
        let level = InterruptLevel::new(self.level()).expect("three bits");
        (level != InterruptLevel::NONE && self.pending_source().is_some()).then_some(
            InterruptRequest {
                level,
                arbitration: self.arbitration(),
            },
        )
    }

    /// IVBA and the pending source's number, $0 for PAB's (GPTRM 7.1.1).
    fn acknowledge(&mut self, level: InterruptLevel, _now: Cycles) -> Option<u8> {
        if level.get() != self.level() {
            return None;
        }
        let source = self.pending_source()?;
        let adjusted = (self.icr >> ICR_PAB_SHIFT) as u8;
        let number = if source == adjusted { 0 } else { source };
        Some((self.icr & ICR_IVBA_MASK) as u8 | number)
    }

    /// Every register to its reset value (GPTRM 9; PLAN.md, "QSM and
    /// GPT on reset"); the board's pins and scheduled edges stay, and
    /// the output edges for the host, with those the reset makes.
    fn reset(&mut self, now: Cycles) {
        self.advance(now);
        let edges = mem::take(&mut self.edges);
        let scheduled = mem::take(&mut self.scheduled);
        *self = Self::with_inputs(now, self.inputs);
        self.edges = Edges {
            pwm_found: Found::new(0, now),
            oc1_found: Found::new(0, now),
            ..edges
        };
        self.scheduled = scheduled;
        self.report_port(now);
        self.find_fast_edges(now);
        self.refresh();
    }

    /// Its registers, timers, PWM and the pins as last seen.
    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(STATE_VERSION, self))
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        let saved: Self = decode(STATE_VERSION, snapshot)?;
        saved.validate()?;
        *self = saved;
        self.refresh();
        Ok(())
    }
}

#[cfg(test)]
mod tests;
