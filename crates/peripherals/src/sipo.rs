//! `74x164`: the 8-bit serial-in, parallel-out shift register
//! (SN74HC164, SCLS115G), on the QSPI, `cascade` of them chained.
//!
//! MOSI is the serial input, A and B (A AND B, B tied high) and SCK
//! is CLK: each rising edge shifts the serial bit into QA and every
//! stage into the next, QH's bit leaving (9.1; Table 1).  Chained, a
//! chip's QH is the next chip's A.  The parallel outputs drive `Q`, a
//! bus net: bit 8k + n is chip k's stage n (QA 0 to QH 7), chip 0 the
//! one MOSI feeds, so the last bit shifted is bit 0 and a 16-bit chain
//! holds a 16-bit transfer as it was sent (in SPI modes 0, 2 and 3:
//! `transfer`).  CLR, the logic input `CLR`,
//! clears every stage while low, whatever CLK does (Table 1).
//!
//! The outputs follow each clock; the peripheral drives `Q` as each transfer
//! ends, the clocks within one not resolved.

use mc683xx_board::inspect::Inspection;
use mc683xx_board::inspect::Radix;
use mc683xx_board::peripheral::Drive;
use mc683xx_board::peripheral::Peripheral;
use mc683xx_board::peripheral::Transfer;
use mc683xx_board::registry::Context;
use mc683xx_board::registry::ParameterError;
use mc683xx_board::registry::PortSpec;
use mc683xx_board::registry::Registry;
use mc683xx_board::registry::RegistryError;
use mc683xx_board::signal::Kind;
use mc683xx_board::signal::Signal;
use mc683xx_board::spi;
use mc683xx_board::spi::Edge;
use mc683xx_board::spi::Edges;
use mc683xx_board::time::Time;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use mc683xx_modules::peripheral::floating_miso;
use serde::Deserialize;
use serde::Serialize;

use crate::octal::LEVELS;
use crate::octal::LINES;
use crate::octal::level;
use crate::shift_register::ShiftRegister;

/// Its registered name.
pub(crate) const NAME: &str = "74x164";

/// The format of its snapshots.
const VERSION: u32 = 2;

/// Its serial edges: each rising edge of CLK shifts A AND B into QA
/// and every stage into the next (9.1; Table 1), QH the last stage,
/// always driven.
const EDGES: Edges = Edges::new(Edge::Rising, Edge::Rising, true);

/// Its ports: the parallel outputs, then CLR.
const Q: usize = 0;
const CLR: usize = 1;

/// The chips chained, one by default; a bus net is 32 bits wide, so at
/// most four.
const CASCADE: &str = "cascade";
const MOST_CASCADED: u8 = 4;
/// CLR's level until a net drives it: high by default, not clearing.
const CLR_LEVEL: &str = "clr";
/// The stages out of power-on, which the datasheet does not give: 0 by
/// default (bit 8k + n is chip k's stage n, as on `Q`).
const INITIAL: &str = "initial";
/// What answers on MISO: `"qh"`, the last chip's QH (by default), or
/// `"none"`, MISO not wired, undriven.
const MISO: &str = "miso";
/// `miso`'s words.
const MISO_WORDS: [(&str, bool); 2] = [("qh", true), ("none", false)];

/// Registers `74x164`.
pub(crate) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    let ports = vec![
        PortSpec::output("Q", Kind::Bus),
        PortSpec::input("CLR", Kind::Logic),
    ];
    registry.register(NAME, ports, |context| {
        Ok(Box::new(Sipo::from_parameters(context)?))
    })
}

/// What it holds, saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    /// Every stage of the chain.
    register: ShiftRegister,
    /// CLR's level: low clears.
    clr: bool,
}

impl State {
    /// Refuses what no run of `width` stages could hold: a stage past
    /// the chain, or one set while CLR is low.
    fn check(self, width: u32) -> Result<(), StateError> {
        if self.register.bits() >> width != 0 {
            return Err(StateError::invalid("a 74x164 stage past its chain"));
        }
        if !self.clr && self.register.bits() != 0 {
            return Err(StateError::invalid("a 74x164 stage set while CLR is low"));
        }
        Ok(())
    }
}

/// Its parameters, saved so that a snapshot restores only into a chain
/// built the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Configuration {
    /// Its stages: eight a chip.
    width: u32,
    /// Whether the last chip's QH answers on MISO.
    qh_on_miso: bool,
}

/// The chain.  The datasheet gives no power-on state: it powers on
/// here holding `initial`, every stage low by default, and CLR low
/// clears it.  It has no reset input but CLR, so RESET
/// changes nothing.
#[derive(Debug)]
struct Sipo {
    /// Its parameters.
    configuration: Configuration,
    /// What it holds.
    state: State,
}

impl Sipo {
    /// From `cascade` (1), `initial` (0), `clr` (`"high"`) and `miso`
    /// (`"qh"`).
    fn from_parameters(context: &Context<'_>) -> Result<Self, ParameterError> {
        let cascade: u8 = context.parameters.integer(CASCADE)?.unwrap_or(1);
        if !(1..=MOST_CASCADED).contains(&cascade) {
            return Err(ParameterError::Range {
                key: CASCADE.to_owned(),
                value: i64::from(cascade),
            });
        }
        let width = u32::from(cascade) * LINES as u32;
        let initial: u64 = context.parameters.integer(INITIAL)?.unwrap_or(0);
        if initial >> width != 0 {
            return Err(ParameterError::invalid(INITIAL, "a stage past the chain's"));
        }
        let clr = level(context, CLR_LEVEL, LEVELS, true)?;
        Ok(Self {
            configuration: Configuration {
                width,
                qh_on_miso: level(context, MISO, MISO_WORDS, true)?,
            },
            state: State {
                // CLR held low clears whatever it powers on with
                register: ShiftRegister::holding(if clr { initial } else { 0 }),
                clr,
            },
        })
    }

    /// Drives `Q` if its stages are not `before`.
    fn drive_outputs(&self, before: u64, drive: &mut Drive<'_>) {
        let bits = self.state.register.bits();
        if bits != before {
            // at most 32 stages, by `cascade`
            drive.drive(Q, Signal::Bus(bits as u32));
        }
    }
}

impl Peripheral for Sipo {
    fn power_on(&mut self, drive: &mut Drive<'_>) {
        // at most 32 stages, by `cascade`
        drive.drive(Q, Signal::Bus(self.state.register.bits() as u32));
    }

    /// Shifts the transfer's bits in on its edges (`EDGES`) in the
    /// transfer's mode.  On MISO, with `miso = "qh"`, the last chip's
    /// QH, its last stage's output (9.2, Functional Block Diagram): the
    /// bits that leave the chain.  In modes 0, 2 and 3 the rise takes
    /// each bit as the master sent it and the master captures QH before
    /// the rise that shifts, so a transfer as long as the chain reads
    /// back the word before.  In mode 1 the rise leads each bit: it
    /// shifts as the master puts the bit on MOSI, so it takes the bit
    /// before (the first, MOSI's level before the transfer), and the
    /// master captures QH on the fall after it, a bit early.  With CLR
    /// low nothing shifts and QH is low (Table 1).
    fn transfer(&mut self, transfer: Transfer, drive: &mut Drive<'_>) -> u16 {
        let before = self.state.register.bits();
        let back = if self.state.clr {
            let mut stages = self.state.register.stages(self.configuration.width);
            spi::exchange(transfer, EDGES, &mut stages)
        } else {
            0
        };
        self.drive_outputs(before, drive);
        if self.configuration.qh_on_miso {
            back
        } else {
            floating_miso(transfer.bits)
        }
    }

    /// No: it drives `Q` from its transfers, and a peripheral that drives a
    /// net in its QSPI calls may not be batched ([docs/board-files.md](https://github.com/charlieh0tel/mc683xx/blob/main/docs/board-files.md),
    /// QSPI batching).
    fn qspi_deferrable(&self) -> bool {
        false
    }

    fn input(&mut self, port: usize, signal: Signal, _: Time, drive: &mut Drive<'_>) {
        if let (CLR, Signal::Level(level)) = (port, signal) {
            self.state.clr = level;
            if !level {
                let before = self.state.register.bits();
                self.state.register.clear();
                self.drive_outputs(before, drive);
            }
        }
    }

    /// The stages, as `Q` carries them, CLR and the chain's length.
    fn inspect(&self) -> Inspection {
        let stages = i64::try_from(self.state.register.bits()).unwrap_or(i64::MAX);
        Inspection::default()
            .integer_in("q", stages, Radix::Binary)
            .boolean("clr", self.state.clr)
            .integer("stages", self.configuration.width)
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(VERSION, &(self.configuration, self.state)))
    }

    /// Refuses another configuration's snapshot and one `State::check`
    /// refuses.
    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        let (configuration, state): (Configuration, State) = decode(VERSION, snapshot)?;
        if configuration != self.configuration {
            return Err(StateError::mismatch("a 74x164 chain built another way"));
        }
        state.check(configuration.width)?;
        self.state = state;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_board::inspect::Value;
    use mc683xx_board::peripheral::Drive;
    use mc683xx_board::peripheral::Peripheral;
    use mc683xx_board::registry::Context;
    use mc683xx_board::registry::Environment;
    use mc683xx_board::registry::ParameterError;
    use mc683xx_board::registry::Parameters;
    use mc683xx_board::signal::Signal;
    use mc683xx_board::time::Time;
    use mc683xx_cpu32::cpu::state::StateError;
    use mc683xx_cpu32::cpu::state::encode;

    use super::CLR;
    use super::Q;
    use super::Sipo;
    use super::State;
    use super::VERSION;
    use crate::spi_mode;

    /// A chain from the parameters `table`, in TOML.
    fn peripheral(table: &str) -> Result<Sipo, ParameterError> {
        let parameters = Parameters::parse(table).unwrap();
        let environment = Environment::default();
        let built = Sipo::from_parameters(&Context::new("sipo", &parameters, &environment));
        assert_eq!(parameters.unread(), None);
        built
    }

    #[test]
    fn initial_is_driven_on_q_at_power_on_and_clr_low_clears_it() {
        let sipo = peripheral("cascade = 2\ninitial = 0x8000").unwrap();
        assert_eq!(sipo.state.register.bits(), 0x8000);
        let cleared = peripheral("cascade = 2\ninitial = 0x8000\nclr = \"low\"").unwrap();
        assert_eq!(cleared.state.register.bits(), 0);
        assert!(peripheral("cascade = 1\ninitial = 0x100").is_err());
    }

    #[test]
    fn it_shows_its_stages() {
        let mut sipo = peripheral("cascade = 2").unwrap();
        shift(&mut sipo, 0xa5c3, 16);
        let inspection = sipo.inspect();
        assert_eq!(inspection.get("q"), Some(&Value::Integer(0xa5c3)));
        assert_eq!(inspection.get("clr"), Some(&Value::Boolean(true)));
        assert_eq!(inspection.get("stages"), Some(&Value::Integer(16)));
    }

    /// Shifts `bits` of `word` in SPI mode 0; gives MISO and what was
    /// driven.
    fn shift(sipo: &mut Sipo, word: u16, bits: u8) -> (u16, Vec<(usize, Signal)>) {
        shift_in(0, sipo, word, bits)
    }

    /// `shift` in SPI mode `mode`.
    fn shift_in(mode: u8, sipo: &mut Sipo, word: u16, bits: u8) -> (u16, Vec<(usize, Signal)>) {
        let mut drive = Drive::new(Time::ZERO);
        let back = sipo.transfer(spi_mode::transfer(mode).shifting(word, bits), &mut drive);
        let driven = drive
            .finish()
            .driven
            .iter()
            .map(|driven| (driven.port, driven.signal))
            .collect();
        (back, driven)
    }

    #[test]
    fn two_chips_hold_a_word_and_give_the_one_before_on_qh() {
        let mut sipo = peripheral("cascade = 2").unwrap();
        assert_eq!(
            shift(&mut sipo, 0xc0a0, 16),
            (0, vec![(Q, Signal::Bus(0xc0a0))])
        );
        assert_eq!(shift(&mut sipo, 0x1234, 16).0, 0xc0a0);
        assert_eq!(
            shift(&mut sipo, 0x56, 8),
            (0x12, vec![(Q, Signal::Bus(0x3456))])
        );
    }

    #[test]
    fn each_mode_but_1_shifts_the_word_as_sent() {
        for mode in [0, 2, 3] {
            let mut sipo = peripheral("cascade = 2").unwrap();
            shift_in(mode, &mut sipo, 0xc0a0, 16);
            assert_eq!(
                shift_in(mode, &mut sipo, 0x1234, 16),
                (0xc0a0, vec![(Q, Signal::Bus(0x1234))]),
                "mode {mode}"
            );
        }
    }

    #[test]
    fn mode_1_shifts_a_bit_late_and_reads_qh_a_bit_early() {
        // the rise leads: it shifts as the master changes MOSI, and the
        // master captures QH on the fall after (9.1; QSMRM 4.3.1)
        let mut sipo = peripheral("cascade = 2").unwrap();
        shift_in(0, &mut sipo, 0xc0a0, 16);
        // MOSI's level before the transfer first, then 0x1234 but its
        // last bit
        let (back, driven) = shift_in(1, &mut sipo, 0x1234, 16);
        assert_eq!(driven, [(Q, Signal::Bus(0x1234 >> 1))]);
        // 0xc0a0 a bit up, then the first bit this transfer shifted in
        assert_eq!(back, 0x8140);
    }

    #[test]
    fn one_chip_keeps_the_last_eight_bits() {
        let mut sipo = peripheral("").unwrap();
        assert_eq!(
            shift(&mut sipo, 0xabcd, 16),
            (0xab, vec![(Q, Signal::Bus(0xcd))])
        );
    }

    #[test]
    fn clr_low_clears_and_holds_every_stage() {
        let mut sipo = peripheral("cascade = 2").unwrap();
        shift(&mut sipo, 0xffff, 16);
        let mut drive = Drive::new(Time::ZERO);
        sipo.input(CLR, Signal::Level(false), Time::ZERO, &mut drive);
        assert_eq!(drive.finish().driven[0].signal, Signal::Bus(0));
        assert_eq!(shift(&mut sipo, 0x1234, 16), (0, vec![]));
        sipo.input(
            CLR,
            Signal::Level(true),
            Time::ZERO,
            &mut Drive::new(Time::ZERO),
        );
        assert_eq!(shift(&mut sipo, 0x1234, 16).1, [(Q, Signal::Bus(0x1234))]);
        let mut tied = peripheral("clr = \"low\"").unwrap();
        assert_eq!(shift(&mut tied, 0xff, 8), (0, vec![]));
    }

    #[test]
    fn without_qh_on_miso_it_floats() {
        let mut sipo = peripheral("cascade = 2\nmiso = \"none\"").unwrap();
        shift(&mut sipo, 0x1234, 16);
        assert_eq!(shift(&mut sipo, 0, 16).0, 0xffff);
        assert_eq!(shift(&mut sipo, 0, 12).0, 0x0fff);
    }

    #[test]
    fn its_parameters_are_checked() {
        assert!(matches!(
            peripheral("cascade = 5"),
            Err(ParameterError::Range { .. })
        ));
        assert!(matches!(
            peripheral("cascade = 0"),
            Err(ParameterError::Range { .. })
        ));
        assert!(matches!(
            peripheral("miso = \"qa\""),
            Err(ParameterError::Invalid { .. })
        ));
    }

    #[test]
    fn it_restores_what_it_saved() {
        let mut sipo = peripheral("cascade = 3").unwrap();
        shift(&mut sipo, 0xbeef, 16);
        let mut other = peripheral("cascade = 3").unwrap();
        other.restore(&sipo.save().unwrap()).unwrap();
        assert_eq!(other.state, sipo.state);
        assert!(other.restore(&[1]).is_err());
    }

    #[test]
    fn it_refuses_another_chain_and_stages_no_run_could_set() {
        let mut sipo = peripheral("cascade = 2").unwrap();
        shift(&mut sipo, 0xbeef, 16);
        let before = sipo.state;
        for other in ["cascade = 3", "cascade = 2\nmiso = \"none\""] {
            let snapshot = peripheral(other).unwrap().save().unwrap();
            assert!(matches!(
                sipo.restore(&snapshot),
                Err(StateError::Mismatch(_))
            ));
        }
        let mut wide = peripheral("cascade = 3").unwrap();
        shift(&mut wide, 0xff, 8);
        shift(&mut wide, 0, 16);
        let past = encode(VERSION, &(sipo.configuration, wide.state));
        let cleared = State {
            clr: false,
            ..before
        };
        for invalid in [past, encode(VERSION, &(sipo.configuration, cleared))] {
            assert!(matches!(
                sipo.restore(&invalid),
                Err(StateError::Invalid(_))
            ));
        }
        assert_eq!(sipo.state, before);
    }

    #[test]
    fn the_format_matches_its_golden() {
        let mut sipo = peripheral("cascade = 2").unwrap();
        shift(&mut sipo, 0xc0a0, 16);
        snapshot_golden::check(
            env!("CARGO_MANIFEST_DIR"),
            "74x164",
            &[("74x164", VERSION)],
            &sipo.save().unwrap(),
        );
    }
}
