//! `mc144110`: Motorola's MC144110, six 6-bit DACs behind one serial
//! input (MC144110/D, Rev. 2).  On the QSPI, its ENB the attachment's
//! select: bits shift in while it is selected, and the frame's end
//! latches them into the six outputs.  Its outputs `out0` to `out5`
//! are analog, R1 Out to R6 Out, in volts.

use mc683xx_board::inspect::Inspection;
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
use serde::Deserialize;
use serde::Serialize;

use crate::dac::Span;
use crate::dac::Volts;
use crate::shift_register::ShiftRegister;

/// Its registered name.
pub(crate) const NAME: &str = "mc144110";

/// The format of its snapshots.
const VERSION: u32 = 2;

/// Its DACs (1, Introduction).
const CHANNELS: usize = 6;
/// A DAC's bits: a word on Din (4.1, Din).
const CODE_BITS: u32 = 6;
/// A word's bits, as a mask.
const CODE_MASK: u64 = (1 << CODE_BITS) - 1;
/// The shift register's stages: six words, 36 clocks (4.1, CLK; Table
/// 4).
const REGISTER_BITS: u32 = 36;
/// Its serial edges: Din shifts in on CLK's fall (4.1, CLK), and Dout,
/// the register's last stage (4.2, Dout; Figure 1), changes as it
/// shifts and is always driven.
const EDGES: Edges = Edges::new(Edge::Falling, Edge::Falling, true);

/// Its parameters: VDD and VSS, in volts, which scale the outputs (4.3;
/// Figure 4).  VDD defaults to 5 V, the first supply Tables 2 and 3
/// characterize; VSS is "usually ground" (4.3).
const VDD: (&str, Volts) = ("vdd", Volts(5.0));
const VSS: (&str, Volts) = ("vss", Volts(0.0));

/// What it holds, saved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    /// The shift register, Din into bit 0, Dout from bit 35 (Figure 1).
    register: ShiftRegister,
    /// The latch the register loads at ENB's rise: the last word
    /// shifted in, bits 5-0, is DAC 1's, the one before DAC 2's, and so
    /// on up (4.1, Din).
    latch: u64,
}

impl State {
    /// DAC `channel`'s code, 0 for DAC 1.
    fn code(self, channel: usize) -> u16 {
        // six bits, by the mask
        (self.latch >> (CODE_BITS as usize * channel) & CODE_MASK) as u16
    }

    /// Refuses a register or a latch wider than its 36 bits.
    fn check(self) -> Result<(), StateError> {
        if (self.register.bits() | self.latch) >> REGISTER_BITS == 0 {
            Ok(())
        } else {
            Err(StateError::invalid("an MC144110 bit past its 36"))
        }
    }
}

/// The peripheral.  No RESET reaches it: the MC144110 has no reset input
/// (Figure 2), so a reset changes nothing.  The datasheet gives no
/// power-on state; it powers on here with every stage and the latch
/// at 0, its outputs at VSS.
#[derive(Debug)]
struct Mc144110 {
    /// Its outputs' span: VSS to VDD.
    span: Span,
    /// What it holds.
    state: State,
}

impl Mc144110 {
    /// From `vdd` and `vss`.
    fn from_parameters(context: &Context<'_>) -> Result<Self, ParameterError> {
        Ok(Self {
            span: Span::from_parameters(context, VSS, VDD)?,
            state: State::default(),
        })
    }

    /// Its ports: `out0` to `out5`, R1 Out to R6 Out.
    fn ports() -> Vec<PortSpec> {
        (0..CHANNELS)
            .map(|channel| PortSpec::output(format!("out{channel}"), Kind::Analog))
            .collect()
    }

    /// Drives each output whose code differs from `before`'s, or every
    /// one without it.
    fn drive_outputs(&self, before: Option<State>, drive: &mut Drive<'_>) {
        for channel in 0..CHANNELS {
            let code = self.state.code(channel);
            if before.is_none_or(|before| before.code(channel) != code) {
                let Volts(volts) = self.span.output(code, CODE_BITS);
                drive.drive(channel, Signal::Analog(volts));
            }
        }
    }
}

impl Peripheral for Mc144110 {
    fn power_on(&mut self, drive: &mut Drive<'_>) {
        self.drive_outputs(None, drive);
    }

    /// Shifts on its edges (`EDGES`) in the transfer's mode, while ENB
    /// is low (4.1, ENB).  In modes 0, 1 and 2 the fall takes each bit
    /// as the master sent it, and the master captures Dout before the
    /// fall that shifts: a frame reads back the register's bits as they
    /// were, the last frame bit for bit.  In mode 3 the fall leads each
    /// bit: it shifts as the master puts the bit on MOSI, so it takes
    /// the bit before (the first, MOSI's level before the transfer),
    /// and the master captures Dout on the rise after it, so the
    /// read-back comes one bit early.
    fn transfer(&mut self, transfer: Transfer, _: &mut Drive<'_>) -> u16 {
        let mut stages = self.state.register.stages(REGISTER_BITS);
        spi::exchange(transfer, EDGES, &mut stages)
    }

    /// ENB's rise, the frame's end, loads the register into the latch
    /// (4.1, ENB).
    fn select(&mut self, _: usize, selected: bool, _: Time, drive: &mut Drive<'_>) {
        if !selected {
            let before = self.state;
            self.state.latch = self.state.register.bits();
            self.drive_outputs(Some(before), drive);
        }
    }

    /// No: it drives its outputs at a frame's end, in `select`, and a
    /// peripheral that drives a net in its QSPI calls may not be batched
    /// ([docs/board-files.md](https://github.com/charlieh0tel/mc683xx/blob/main/docs/board-files.md),
    /// QSPI batching).
    fn qspi_deferrable(&self) -> bool {
        false
    }

    /// The shift register, and each output's code and volts.
    fn inspect(&self) -> Inspection {
        let register = i64::try_from(self.state.register.bits()).unwrap_or(i64::MAX);
        (0..CHANNELS).fold(
            Inspection::default().register("register", register),
            |shown, channel| {
                let code = self.state.code(channel);
                let Volts(volts) = self.span.output(code, CODE_BITS);
                shown.group(
                    format!("out{channel}"),
                    Inspection::default()
                        .register("code", code)
                        .real("volts", volts, "V"),
                )
            },
        )
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(VERSION, &(self.span, self.state)))
    }

    /// Refuses another span's snapshot and one `State::check` refuses.
    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        let (span, state): (Span, State) = decode(VERSION, snapshot)?;
        if span != self.span {
            return Err(StateError::mismatch("an MC144110 of other supplies"));
        }
        state.check()?;
        self.state = state;
        Ok(())
    }
}

/// Registers `mc144110`.
pub(crate) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register(NAME, Mc144110::ports(), |context| {
        Ok(Box::new(Mc144110::from_parameters(context)?))
    })
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

    use super::Mc144110;
    use super::State;
    use super::VERSION;
    use crate::spi_mode;

    /// A peripheral from the parameters `table`, in TOML.
    fn peripheral(table: &str) -> Result<Mc144110, ParameterError> {
        let parameters = Parameters::parse(table).unwrap();
        let environment = Environment::default();
        let built = Mc144110::from_parameters(&Context::new("dac", &parameters, &environment));
        assert_eq!(parameters.unread(), None);
        built
    }

    #[test]
    fn it_shows_each_output_s_code_and_volts() {
        let mut dac = peripheral("").unwrap();
        // DAC 1's code 63, DAC 2's 32 (4.1, Din)
        dac.state.latch = 32 << 6 | 63;
        let inspection = dac.inspect();
        let Some(Value::Group(first)) = inspection.get("out0") else {
            panic!("out0");
        };
        assert_eq!(first.get("code"), Some(&Value::Integer(63)));
        assert_eq!(first.get("volts"), Some(&Value::Real(5.0 * 63.0 / 64.0)));
        let Some(Value::Group(second)) = inspection.get("out1") else {
            panic!("out1");
        };
        assert_eq!(second.get("volts"), Some(&Value::Real(2.5)));
        assert_eq!(inspection.fields.len(), 7);
    }

    /// Shifts `words`, `bits` each, in one frame; gives what came back
    /// and what was driven at its end, by port.  In mode 2, the
    /// smartclock firmware's to write it.
    fn frame(dac: &mut Mc144110, words: &[u16], bits: u8) -> (Vec<u16>, Vec<(usize, Signal)>) {
        frame_in(2, dac, words, bits)
    }

    /// `frame` in SPI mode `mode`.
    fn frame_in(
        mode: u8,
        dac: &mut Mc144110,
        words: &[u16],
        bits: u8,
    ) -> (Vec<u16>, Vec<(usize, Signal)>) {
        let mut drive = Drive::new(Time::ZERO);
        dac.select(0, true, Time::ZERO, &mut drive);
        let back = words
            .iter()
            .enumerate()
            .map(|(index, &word)| {
                let transfer = spi_mode::transfer(mode).shifting(word, bits);
                let transfer = if index == 0 {
                    transfer
                } else {
                    transfer.continuing()
                };
                dac.transfer(transfer, &mut drive)
            })
            .collect();
        dac.select(0, false, Time::ZERO, &mut drive);
        let driven = drive
            .finish()
            .driven
            .iter()
            .map(|driven| (driven.port, driven.signal))
            .collect();
        (back, driven)
    }

    /// The volts for `code` at the default 5 V.
    fn volts(code: u8) -> Signal {
        Signal::Analog(5.0 * f64::from(code) / 64.0)
    }

    #[test]
    fn the_last_word_shifted_is_dac_ones() {
        let mut dac = peripheral("").unwrap();
        // DAC 6 first, DAC 1 last, two to a 12-bit word
        let (_, driven) = frame(&mut dac, &[63 << 6 | 50, 40 << 6 | 20, 10 << 6 | 1], 12);
        assert_eq!(
            driven,
            [
                (0, volts(1)),
                (1, volts(10)),
                (2, volts(20)),
                (3, volts(40)),
                (4, volts(50)),
                (5, volts(63)),
            ]
        );
    }

    #[test]
    fn only_the_outputs_that_change_are_driven() {
        let mut dac = peripheral("").unwrap();
        frame(&mut dac, &[0, 0, 10 << 6 | 1], 12);
        let (_, driven) = frame(&mut dac, &[0, 0, 11 << 6 | 1], 12);
        assert_eq!(driven, [(1, volts(11))]);
    }

    #[test]
    fn nothing_latches_before_enb_rises() {
        let mut dac = peripheral("").unwrap();
        let mut drive = Drive::new(Time::ZERO);
        dac.select(0, true, Time::ZERO, &mut drive);
        dac.transfer(spi_mode::transfer(2).shifting(0xfff, 12), &mut drive);
        assert!(drive.finish().driven.is_empty());
        assert_eq!(dac.state.latch, 0);
    }

    #[test]
    fn fewer_words_reach_the_low_dacs_first() {
        // Table 4: six clocks set Q1/R1; the rest shift up
        let mut dac = peripheral("").unwrap();
        frame(&mut dac, &[0x2a << 6 | 0x15], 12);
        assert_eq!(dac.state.code(0), 0x15);
        assert_eq!(dac.state.code(1), 0x2a);
        frame(&mut dac, &[0x3f], 8);
        assert_eq!(dac.state.code(0), 0x3f);
        // eight bits in: each code two bits up into the next DAC's
        assert_eq!(dac.state.code(1), 0x15 << 2 & 0x3f);
        assert_eq!(dac.state.code(2), (0x2a << 2 & 0x3f) | 0x15 >> 4);
    }

    #[test]
    fn dout_gives_back_the_last_frame_bit_for_bit_but_in_mode_3() {
        let words = [0x555, 0x123, 0x456];
        for mode in [0, 1, 2] {
            let mut dac = peripheral("").unwrap();
            assert_eq!(frame_in(mode, &mut dac, &words, 12).0, [0, 0, 0]);
            assert_eq!(dac.state.register.bits(), 0x555_123_456, "mode {mode}");
            assert_eq!(
                frame_in(mode, &mut dac, &[0xaa, 0xbb, 0xcc], 12).0,
                words,
                "mode {mode}"
            );
        }
    }

    #[test]
    fn mode_3_reads_back_a_bit_early_and_shifts_in_a_bit_late() {
        // the fall leads: it shifts as the master changes MOSI, and the
        // master captures Dout on the rise after (4.1, CLK; QSMRM 4.3.1)
        let mut dac = peripheral("").unwrap();
        frame_in(2, &mut dac, &[0x555, 0x555, 0x555], 12);
        let (back, _) = frame_in(3, &mut dac, &[0x555, 0x555, 0x555], 12);
        // each word the one written a bit up, with the next one's first
        // bit; the last, the first bit this frame shifted in
        assert_eq!(back, [0xaaa, 0xaaa, 0xaaa]);
        let shifted: Vec<u16> = back.iter().map(|word| word >> 1).collect();
        assert_eq!(shifted, [0x555; 3], "as the firmware shifts it");
        // each word shifted in a bit late, its first bit MOSI's level
        // before the transfer
        assert_eq!(dac.state.register.bits(), 0x2aa_2aa_2aa);
    }

    #[test]
    fn vdd_and_vss_scale_the_outputs() {
        let mut dac = peripheral("vdd = 12.0\nvss = 2").unwrap();
        let (_, driven) = frame(&mut dac, &[0, 0, 32], 12);
        assert_eq!(driven[0], (0, Signal::Analog(7.0)), "halfway");
        assert!(matches!(
            peripheral("vdd = nan"),
            Err(ParameterError::Invalid { .. })
        ));
        assert!(matches!(
            peripheral("vdd = \"high\""),
            Err(ParameterError::Type { .. })
        ));
    }

    #[test]
    fn power_on_drives_every_output_at_vss() {
        let mut dac = peripheral("vss = -1").unwrap();
        let mut drive = Drive::new(Time::ZERO);
        dac.power_on(&mut drive);
        let driven = drive.finish().driven;
        assert_eq!(driven.len(), 6);
        assert!(
            driven
                .iter()
                .all(|driven| driven.signal == Signal::Analog(-1.0))
        );
    }

    #[test]
    fn it_restores_what_it_saved() {
        let mut dac = peripheral("").unwrap();
        frame(&mut dac, &[1, 2, 3], 12);
        frame(&mut dac, &[4], 12);
        let snapshot = dac.save().unwrap();
        let mut other = peripheral("").unwrap();
        other.restore(&snapshot).unwrap();
        assert_eq!(other.state, dac.state);
        assert!(other.restore(&snapshot[..2]).is_err());
        assert!(other.restore(&[9, 0, 0, 0]).is_err(), "another version");
    }

    #[test]
    fn it_refuses_other_supplies_and_bits_past_36() {
        let mut dac = peripheral("").unwrap();
        frame(&mut dac, &[1, 2, 3], 12);
        let before = dac.state;
        let other = peripheral("vdd = 12.0").unwrap();
        assert!(matches!(
            dac.restore(&other.save().unwrap()),
            Err(StateError::Mismatch(_))
        ));
        let wide = State {
            latch: 1 << 36,
            ..before
        };
        // the register as its one field, every bit set
        let invalid = [
            encode(VERSION, &(dac.span, wide)),
            encode(VERSION, &(dac.span, (u64::MAX, 0_u64))),
        ];
        for invalid in invalid {
            assert!(matches!(dac.restore(&invalid), Err(StateError::Invalid(_))));
        }
        assert_eq!(dac.state, before);
    }

    #[test]
    fn the_format_matches_its_golden() {
        let mut dac = peripheral("").unwrap();
        frame(&mut dac, &[0x555, 0x123, 0x456], 12);
        frame(&mut dac, &[0xabc], 12);
        snapshot_golden::check(
            env!("CARGO_MANIFEST_DIR"),
            "mc144110",
            &[("mc144110", VERSION)],
            &dac.save().unwrap(),
        );
    }
}
