//! `adc0838`: Texas Instruments' (formerly National Semiconductor's)
//! ADC0838, an 8-bit successive-approximation converter with an
//! 8-channel multiplexer and a serial interface, on the QSPI.  Cited
//! as SNAS531B, its datasheet ("ADC0831-N/ADC0832-N/ADC0834-N/
//! ADC0838-N 8-Bit Serial I/O A/D Converters with Multiplexer
//! Options", `third_party/datasheets/ADC0838.pdf`), by section.
//!
//! Its ports are the analog inputs `ch0` to `ch7` and `com`, in volts,
//! and the logic input `se` (/SE).  An input no net drives reads 0 V,
//! and /SE no net drives is low, as a board that ties it to ground.
//! Parameters: `vref_volts`, the reference (default 5.0), and
//! `noise_volts`, the standard deviation of noise added to each
//! conversion's input (default 0, none), drawn from the peripheral's own
//! seed.  [`docs/peripherals.md`](https://github.com/charlieh0tel/mc683xx/blob/main/docs/peripherals.md) gives the choices where the datasheet
//! is silent.

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
use mc683xx_board::spi::Slave;
use mc683xx_board::time::Time;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use serde::Deserialize;
use serde::Serialize;

use crate::noise::Noise;

/// Its registered name.
pub(crate) const NAME: &str = "adc0838";

/// The format of its snapshots.
const VERSION: u32 = 3;

/// The analog channels (SNAS531B, Table 1).
const CHANNELS: usize = 8;
/// The port of COM, the "-" input of every single-ended channel
/// (SNAS531B, Table 2): after the channels.
const COM: usize = CHANNELS;
/// The port of /SE.
const SHIFT_ENABLE: usize = COM + 1;

/// The reference when the board file gives none: VCC's typical 5 V
/// (SNAS531B, Reference Considerations).
const DEFAULT_VREF_VOLTS: f64 = 5.0;
/// The output codes over the reference's span: 1 LSB is VREF / 256
/// (SNAS531B, Reference Considerations).
const CODES: f64 = 256.0;
/// The highest output code.
const FULL_SCALE: f64 = 255.0;

/// The bits of the MUX address after the start bit: SGL/DIF, ODD/SIGN,
/// SELECT 1 and SELECT 0, in that order (SNAS531B, Table 2, Figure 21).
const ADDRESS_BITS: u8 = 4;
/// SGL/DIF in the address: set for single-ended.
const SINGLE_ENDED: u8 = 0b1000;
/// ODD/SIGN in the address.
const ODD_SIGN: u8 = 0b0100;
/// SELECT 1 and SELECT 0: the channel pair.
const SELECT: u8 = 0b0011;

/// DO's falling edges after the address, by what each drives (SNAS531B,
/// THE DIGITAL INTERFACE steps 5 to 8, Figure 21): the first the
/// leading zero of the MUX settling time.
const LEADING_ZERO_EDGE: u8 = 1;
/// The edge that drives D0, the LSB, the last of MSB first.  With /SE
/// high DO holds it there.
const LSB_EDGE: u8 = LEADING_ZERO_EDGE + 8;
/// The edge that drives D7 again, the last of LSB first; after it DO
/// stays low until CS goes high.
const LAST_EDGE: u8 = LSB_EDGE + 7;

/// Its serial edges: DI clocked in on the rising edge, DO changing on
/// the falling edge, and DO in TRI-STATE until the falling edge after
/// the address (SNAS531B, THE DIGITAL INTERFACE steps 3, 5 and 6); in
/// TRI-STATE MISO floats, read as ones.
const EDGES: Edges = Edges::new(Edge::Rising, Edge::Falling, false);

/// Registers the type in `registry`.
pub(crate) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register(NAME, ports(), |context| {
        Ok(Box::new(Adc0838::from_parameters(context)?))
    })
}

/// Its ports: the channels in order, COM, then /SE.
fn ports() -> Vec<PortSpec> {
    (0..CHANNELS)
        .map(|channel| PortSpec::input(format!("ch{channel}"), Kind::Analog))
        .chain([
            PortSpec::input("com", Kind::Analog),
            PortSpec::input("se", Kind::Logic),
        ])
        .collect()
}

/// The converter's serial state within a CS low frame (SNAS531B, THE
/// DIGITAL INTERFACE).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
    /// Waiting for the start bit, the first one on DI; DO in TRI-STATE.
    Idle,
    /// Shifting in the MUX address.
    Addressing {
        /// The bits so far, the first in the highest place.
        address: u8,
        /// How many.
        count: u8,
    },
    /// Converted: DI is ignored and DO shifts the result out.
    Output {
        /// The 8-bit result.
        result: u8,
        /// DO's falling edges since the last address bit: 0 until the
        /// first, which drives `LEADING_ZERO_EDGE`.
        edges: u8,
    },
}

impl Phase {
    /// What DO drives now: `None` in TRI-STATE.
    fn data_out(self) -> Option<bool> {
        let Self::Output { result, edges } = self else {
            return None;
        };
        let bit = |index: u8| Some(result >> index & 1 != 0);
        match edges {
            0 => None,
            LEADING_ZERO_EDGE => Some(false),
            // MSB first: D7 down to D0
            edges if edges <= LSB_EDGE => bit(LSB_EDGE - edges),
            // then LSB first, D0 not repeated: D1 up to D7
            edges if edges <= LAST_EDGE => bit(edges - LSB_EDGE),
            _ => Some(false),
        }
    }
}

impl Phase {
    /// Refuses what no frame reaches: more address bits than the
    /// address has, or DO past the edge after the last.
    fn check(self) -> Result<(), StateError> {
        let valid = match self {
            Self::Idle => true,
            Self::Addressing { address, count } => count < ADDRESS_BITS && address >> count == 0,
            Self::Output { edges, .. } => edges <= LAST_EDGE + 1,
        };
        if valid {
            Ok(())
        } else {
            Err(StateError::invalid("an ADC0838 frame past its end"))
        }
    }
}

/// Its parameters, saved so that a snapshot restores only into a
/// converter built the same way.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct Configuration {
    /// The reference, in volts: the span of the 256 codes.
    vref_volts: f64,
    /// The noise's standard deviation, in volts; 0 for none.
    noise_volts: f64,
}

/// What a snapshot holds after the parameters: what changes as it
/// runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct State {
    /// The serial state.
    phase: Phase,
    /// /SE's level: high holds D0 on DO.
    shift_enable: bool,
    /// The noise's generator.
    noise: Noise,
}

/// The converter.
#[derive(Debug)]
struct Adc0838 {
    /// Its parameters.
    configuration: Configuration,
    /// Its state.
    state: State,
}

impl Adc0838 {
    /// The converter with reference `vref_volts`, input noise of
    /// standard deviation `noise_volts`, and its noise from `seed`.
    fn new(vref_volts: f64, noise_volts: f64, seed: u64) -> Self {
        Self {
            configuration: Configuration {
                vref_volts,
                noise_volts,
            },
            state: State {
                phase: Phase::Idle,
                shift_enable: false,
                noise: Noise::new(seed),
            },
        }
    }

    /// From its parameters, its noise seeded from the peripheral's name.
    fn from_parameters(context: &Context<'_>) -> Result<Self, ParameterError> {
        let parameter = |key: &str, default: f64, valid: fn(f64) -> bool, reason: &str| {
            let value = context.parameters.float(key)?.unwrap_or(default);
            if valid(value) {
                Ok(value)
            } else {
                Err(ParameterError::invalid(key, reason))
            }
        };
        let vref_volts = parameter(
            "vref_volts",
            DEFAULT_VREF_VOLTS,
            |volts| volts.is_finite() && volts > 0.0,
            "not a positive voltage",
        )?;
        let noise_volts = parameter(
            "noise_volts",
            0.0,
            |volts| volts.is_finite() && volts >= 0.0,
            "not a voltage of zero or more",
        )?;
        Ok(Self::new(
            vref_volts,
            noise_volts,
            context.environment.seed_for(context.name),
        ))
    }

    /// The conversion `address` selects, of the inputs now.
    fn convert(&mut self, address: u8, drive: &Drive<'_>) -> u8 {
        let (plus, minus) = inputs(address);
        let level = |port| drive.sample(port).unwrap_or(0.0);
        let Configuration {
            vref_volts,
            noise_volts,
        } = self.configuration;
        let noise = if noise_volts > 0.0 {
            noise_volts * self.state.noise.normal()
        } else {
            0.0
        };
        quantize(level(plus) - level(minus) + noise, vref_volts)
    }
}

/// The "+" and "-" input ports `address` selects (SNAS531B, Tables 2
/// and 3): single-ended, a channel against COM, ODD/SIGN choosing the
/// odd channel of the pair SELECT names; differential, the pair, the
/// even channel "+" unless SIGN.
fn inputs(address: u8) -> (usize, usize) {
    let even = 2 * usize::from(address & SELECT);
    let odd = address & ODD_SIGN != 0;
    match (address & SINGLE_ENDED != 0, odd) {
        (true, _) => (even + usize::from(odd), COM),
        (false, false) => (even, even + 1),
        (false, true) => (even + 1, even),
    }
}

/// The code for `volts` between "+" and "-" against `vref_volts`: 1 LSB
/// is VREF / 256 (SNAS531B, Reference Considerations), the first
/// transition, 00 to 01, at 1/2 LSB and the last, FE to FF, at VREF
/// less 1 1/2 LSB (Optional Adjustments, Zero Error and Full-Scale), so
/// the code is the nearest whole number of LSBs.  "+" below "-" is all
/// zeros (Multiplexer Addressing).
#[expect(clippy::cast_sign_loss, reason = "clamped to 0 through 255 first")]
fn quantize(volts: f64, vref_volts: f64) -> u8 {
    let code = (volts / vref_volts * CODES + 0.5).floor();
    if code.is_nan() {
        return 0;
    }
    code.clamp(0.0, FULL_SCALE) as u8
}

/// The converter's serial side during a transfer, with the inputs it
/// converts.
struct Serial<'a, 'b> {
    /// The converter.
    adc: &'a mut Adc0838,
    /// The call's drive, to sample the inputs.
    drive: &'a Drive<'b>,
}

impl Slave for Serial<'_, '_> {
    /// DI into the MUX address on the rising edge (SNAS531B, THE
    /// DIGITAL INTERFACE step 3); the last address bit converts.
    fn sample(&mut self, data_in: bool) {
        self.adc.state.phase = match self.adc.state.phase {
            Phase::Idle if data_in => Phase::Addressing {
                address: 0,
                count: 0,
            },
            Phase::Addressing { address, count } => {
                let address = address << 1 | u8::from(data_in);
                let count = count + 1;
                if count == ADDRESS_BITS {
                    Phase::Output {
                        result: self.adc.convert(address, self.drive),
                        edges: 0,
                    }
                } else {
                    Phase::Addressing { address, count }
                }
            }
            phase => phase,
        };
    }

    /// DO changes on the falling edge (steps 5 to 8): the first after
    /// the address drives the leading zero, and /SE high holds D0.
    fn change(&mut self) {
        let state = &mut self.adc.state;
        if let Phase::Output { result, edges } = state.phase {
            let held = edges == LSB_EDGE && state.shift_enable;
            state.phase = Phase::Output {
                result,
                edges: if held {
                    edges
                } else {
                    edges.saturating_add(1).min(LAST_EDGE + 1)
                },
            };
        }
    }

    fn miso(&self) -> Option<bool> {
        self.adc.state.phase.data_out()
    }
}

impl Peripheral for Adc0838 {
    /// Shifts on its edges (`EDGES`) in the transfer's mode.  In modes
    /// 0 and 3, the edges it is made for, each rise takes the DI bit
    /// the master sent for it and the master captures DO on the rise,
    /// as the fall before drove it.  In mode 2 the master captures DO
    /// on the fall that changes it, so it reads each bit a clock late,
    /// a TRI-STATE one first.  In mode 1 the rise leads each bit: the
    /// master changes DI on it, so the converter takes the bit before
    /// (the first, MOSI's level before the transfer) and the start bit
    /// comes a clock late, but the master captures on the fall after
    /// that rise, so the reply comes as in mode 2.
    fn transfer(&mut self, transfer: Transfer, drive: &mut Drive<'_>) -> u16 {
        let mut serial = Serial { adc: self, drive };
        spi::exchange(transfer, EDGES, &mut serial)
    }

    /// CS high clears its registers; CS low starts a frame waiting for
    /// the start bit (SNAS531B, THE DIGITAL INTERFACE steps 1 and 9).
    fn select(&mut self, _: usize, _: bool, _: Time, _: &mut Drive<'_>) {
        self.state.phase = Phase::Idle;
    }

    /// No: a conversion samples its inputs (`Drive::sample`), nets other
    /// peripherals drive, which must be read at the transfer's time.
    fn qspi_deferrable(&self) -> bool {
        false
    }

    fn input(&mut self, port: usize, signal: Signal, _: Time, _: &mut Drive<'_>) {
        if let (SHIFT_ENABLE, Signal::Level(level)) = (port, signal) {
            self.state.shift_enable = level;
        }
    }

    /// The frame's phase, its address or result, DO, /SE and the
    /// reference.
    fn inspect(&self) -> Inspection {
        let phase = self.state.phase;
        let shown = match phase {
            Phase::Idle => Inspection::default().text("phase", "idle"),
            Phase::Addressing { address, count } => Inspection::default()
                .text("phase", "addressing")
                .register("address", address)
                .integer("address_bits", count),
            Phase::Output { result, edges } => Inspection::default()
                .text("phase", "output")
                .register("result", result)
                .integer("do_edges", edges),
        };
        let shown = match phase.data_out() {
            Some(level) => shown.boolean("do", level),
            None => shown.text("do", "tri-state"),
        };
        shown.boolean("se", self.state.shift_enable).real(
            "vref",
            self.configuration.vref_volts,
            "V",
        )
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(VERSION, &(self.configuration, &self.state)))
    }

    /// Refuses another configuration's snapshot and a frame
    /// `Phase::check` refuses.
    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        let (configuration, state): (Configuration, State) = decode(VERSION, snapshot)?;
        if configuration != self.configuration {
            return Err(StateError::mismatch("an ADC0838 built another way"));
        }
        state.phase.check()?;
        self.state = state;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_board::inspect::Value;
    use mc683xx_board::peripheral::Drive;
    use mc683xx_board::peripheral::Peripheral;
    use mc683xx_board::peripheral::Transfer;
    use mc683xx_board::registry::Context;
    use mc683xx_board::registry::Environment;
    use mc683xx_board::registry::ParameterError;
    use mc683xx_board::registry::Parameters;
    use mc683xx_board::signal::Signal;
    use mc683xx_board::time::Time;
    use mc683xx_cpu32::cpu::state::StateError;
    use mc683xx_cpu32::cpu::state::encode;

    use super::Adc0838;
    use super::CHANNELS;
    use super::COM;
    use super::Configuration;
    use super::NAME;
    use super::Phase;
    use super::SHIFT_ENABLE;
    use super::State;
    use super::VERSION;
    use super::quantize;
    use crate::spi_mode;

    /// The inputs a test drives, by port: the channels, then COM.
    type Inputs = [Option<f64>; CHANNELS + 1];

    /// A peripheral from the board file parameters `table`, seeded from
    /// `seed`.
    fn built(table: &str, seed: u64) -> Result<Adc0838, ParameterError> {
        let parameters = Parameters::parse(table).unwrap();
        let mut environment = Environment::default();
        environment.seed = seed;
        let peripheral = Adc0838::from_parameters(&Context::new(NAME, &parameters, &environment));
        assert_eq!(parameters.unread(), None);
        peripheral
    }

    /// A peripheral with a 5 V reference and no noise.
    fn adc() -> Adc0838 {
        built("", 0).unwrap()
    }

    #[test]
    fn it_shows_its_frame() {
        let mut adc = adc();
        let text = |value: &str| Some(Value::Text(value.to_owned()));
        assert_eq!(adc.inspect().get("phase").cloned(), text("idle"));
        assert_eq!(adc.inspect().get("do").cloned(), text("tri-state"));
        adc.state.phase = Phase::Output {
            result: 0x80,
            edges: 2,
        };
        let inspection = adc.inspect();
        assert_eq!(inspection.get("phase").cloned(), text("output"));
        assert_eq!(inspection.get("result"), Some(&Value::Integer(0x80)));
        assert_eq!(inspection.get("do"), Some(&Value::Boolean(true)), "D7");
        assert_eq!(inspection.get("vref"), Some(&Value::Real(5.0)));
    }

    /// One frame in SPI mode 3, the smartclock firmware's: CS low, a
    /// transfer of `bits` of `out` for each, CS high; the replies.
    fn frame(adc: &mut Adc0838, inputs: Inputs, transfers: &[(u16, u8)]) -> Vec<u16> {
        frame_in(3, adc, inputs, transfers)
    }

    /// The frame's transfer of `bits` of `out` in SPI mode `mode`, its
    /// first if `index` is 0.
    fn transfer(mode: u8, index: usize, out: u16, bits: u8) -> Transfer {
        let transfer = spi_mode::transfer(mode).shifting(out, bits);
        if index == 0 {
            transfer
        } else {
            transfer.continuing()
        }
    }

    /// `frame` in SPI mode `mode`.
    fn frame_in(mode: u8, adc: &mut Adc0838, inputs: Inputs, transfers: &[(u16, u8)]) -> Vec<u16> {
        let sampler = move |port: usize, _: Time| inputs.get(port).copied().flatten();
        let mut drive = Drive::with_sampler(Time(0), &sampler);
        adc.select(0, true, Time(0), &mut drive);
        let replies = transfers
            .iter()
            .enumerate()
            .map(|(index, &(out, bits))| adc.transfer(transfer(mode, index, out, bits), &mut drive))
            .collect();
        adc.select(0, false, Time(0), &mut drive);
        assert!(drive.finish().driven.is_empty(), "it drives nothing");
        replies
    }

    /// The command for a start bit then `address`, in the top five bits
    /// of a 16-bit word.
    fn command(address: u8) -> u16 {
        (0b1_0000 | u16::from(address)) << 11
    }

    /// The 16-bit reply to `command(address)` for `result`: five bits
    /// in TRI-STATE, the leading zero, D7 to D0, then D1 and D2.
    fn reply(result: u8) -> u16 {
        let result = u16::from(result);
        0b1_1111 << 11 | result << 2 | (result >> 1 & 1) << 1 | result >> 2 & 1
    }

    /// The code of `volts` on 5 V.
    fn code(volts: f64) -> u8 {
        quantize(volts, 5.0)
    }

    #[test]
    fn single_ended_selects_each_channel_against_com() {
        // Table 2: ODD/SIGN the low bit of the channel, SELECT its pair
        let addresses = [
            0b1000, 0b1100, 0b1001, 0b1101, 0b1010, 0b1110, 0b1011, 0b1111,
        ];
        let mut inputs: Inputs = [None; CHANNELS + 1];
        for (channel, input) in inputs.iter_mut().take(CHANNELS).enumerate() {
            *input = Some(0.5 + 0.5 * channel as f64);
        }
        inputs[COM] = Some(0.25);
        let mut adc = adc();
        for (channel, address) in addresses.into_iter().enumerate() {
            let expected = code(0.25 + 0.5 * channel as f64);
            assert_eq!(
                frame(&mut adc, inputs, &[(command(address), 16)]),
                [reply(expected)],
                "channel {channel}"
            );
        }
    }

    #[test]
    fn modes_0_and_3_read_as_made_and_modes_1_and_2_late() {
        // DI on the rise, DO on the fall (THE DIGITAL INTERFACE steps 3
        // and 6)
        let mut inputs: Inputs = [None; CHANNELS + 1];
        inputs[2] = Some(3.3);
        let expected = reply(code(3.3));
        let mut adc = adc();
        let mut read = |mode| frame_in(mode, &mut adc, inputs, &[(command(0b1001), 16)]);
        assert_eq!(read(0), [expected]);
        assert_eq!(read(3), [expected]);
        // the master captures DO on the fall that changes it: a clock
        // late, a TRI-STATE one first
        assert_eq!(read(2), [expected >> 1 | 0x8000]);
        // mode 1 changes DI on the rise that samples it, the start bit
        // a clock late, but captures on the fall after that rise
        assert_eq!(read(1), [expected >> 1 | 0x8000]);
    }

    #[test]
    fn differential_selects_a_pair_and_its_sign() {
        // Table 3: SELECT the pair, SIGN which of it is "+"
        let mut inputs: Inputs = [None; CHANNELS + 1];
        inputs[4] = Some(3.0);
        inputs[5] = Some(1.0);
        let mut adc = adc();
        assert_eq!(
            frame(&mut adc, inputs, &[(command(0b0010), 16)]),
            [reply(code(2.0))]
        );
        // "+" below "-": all zeros
        assert_eq!(
            frame(&mut adc, inputs, &[(command(0b0110), 16)]),
            [reply(0)]
        );
        inputs[COM] = Some(1.0);
        assert_eq!(
            frame(&mut adc, inputs, &[(command(0b0110), 16)]),
            [reply(0)],
            "COM takes no peripheral"
        );
        inputs[4] = Some(1.0);
        inputs[5] = Some(2.5);
        assert_eq!(
            frame(&mut adc, inputs, &[(command(0b0110), 16)]),
            [reply(code(1.5))]
        );
    }

    #[test]
    fn the_codes_change_half_an_lsb_either_side_of_each_step() {
        let lsb = 5.0 / 256.0;
        assert_eq!(code(0.0), 0);
        assert_eq!(code(0.49 * lsb), 0);
        assert_eq!(code(0.51 * lsb), 1);
        assert_eq!(code(100.49 * lsb), 100);
        assert_eq!(code(100.51 * lsb), 101);
        assert_eq!(code(5.0 - 1.51 * lsb), 0xfe);
        assert_eq!(code(5.0 - 1.49 * lsb), 0xff);
        assert_eq!(code(6.0), 0xff);
        assert_eq!(code(-1.0), 0);
        assert_eq!(code(f64::NAN), 0);
        assert_eq!(quantize(1.0, 2.0), 0x80, "the reference sets the span");
    }

    #[test]
    fn leading_zeros_come_before_the_start_bit_and_the_output_spans_transfers() {
        // Figure 21 in 8-bit transfers: three zeros, the start bit and
        // channel 3's address in the first, then 19 clocks of output
        let mut inputs: Inputs = [None; CHANNELS + 1];
        inputs[3] = Some(4.0);
        let result = code(4.0);
        let mut adc = adc();
        let replies = frame(
            &mut adc,
            inputs,
            &[(0b0001_1101, 8), (0b1111_1111, 8), (0, 8)],
        );
        let stream = replies
            .iter()
            .fold(0_u32, |stream, &reply| stream << 8 | u32::from(reply));
        let result = u32::from(result);
        let lsb_first = (0..8).fold(0, |bits, bit| bits << 1 | (result >> bit & 1));
        // eight in TRI-STATE, the leading zero, MSB first, LSB first
        // without D0 again, then zeros
        let expected = 0xff << 16 | result << 7 | (lsb_first & 0x7f);
        assert_eq!(stream, expected, "{stream:024b}");
    }

    #[test]
    fn do_goes_low_after_lsb_first_until_cs_goes_high() {
        let mut inputs: Inputs = [None; CHANNELS + 1];
        inputs[0] = Some(5.0);
        let mut adc = adc();
        let replies = frame(&mut adc, inputs, &[(command(0b1000), 16), (0, 16), (0, 16)]);
        // D3 to D7 LSB first, then low
        assert_eq!(replies[1], 0b1_1111 << 11);
        assert_eq!(replies[2], 0);
        // CS high cleared it: a new frame waits for a start bit
        assert_eq!(frame(&mut adc, inputs, &[(0, 16)]), [0xffff]);
    }

    #[test]
    fn a_frame_without_its_whole_address_converts_nothing() {
        let mut adc = adc();
        let inputs: Inputs = [Some(1.0); CHANNELS + 1];
        assert_eq!(frame(&mut adc, inputs, &[(0b0_0011, 5)]), [0b1_1111]);
        assert_eq!(adc.state.phase, Phase::Idle);
        assert_eq!(
            frame(&mut adc, inputs, &[(command(0b0000), 16)]),
            [reply(0)],
            "a fresh frame after CS high"
        );
    }

    #[test]
    fn se_high_holds_d0_until_it_goes_low() {
        // in mode 0 the master reads D0 again before the next fall; in
        // mode 3 a transfer starts with a fall, so it reads D1 first
        for (mode, after) in [(0, 0b1010_0101), (3, 0b0100_1010)] {
            se_holds_d0(mode, after);
        }
    }

    /// The SE test in SPI mode `mode`: `after` the last transfer's
    /// reply, SE low.
    fn se_holds_d0(mode: u8, after: u16) {
        let mut inputs: Inputs = [None; CHANNELS + 1];
        inputs[1] = Some(code_volts(0b1010_0101));
        let mut adc = adc();
        let mut drive = Drive::new(Time(0));
        adc.input(SHIFT_ENABLE, Signal::Level(true), Time(0), &mut drive);
        let sampler = move |port: usize, _: Time| inputs.get(port).copied().flatten();
        let mut drive = Drive::with_sampler(Time(0), &sampler);
        adc.select(0, true, Time(0), &mut drive);
        // the start bit, channel 1, the leading zero, MSB first
        let first = adc.transfer(transfer(mode, 0, 0b1_1100 << 9, 14), &mut drive);
        assert_eq!(first, 0b1_1111 << 9 | 0b1010_0101);
        assert_eq!(
            adc.transfer(transfer(mode, 1, 0, 4), &mut drive),
            0b1111,
            "D0 held"
        );
        adc.input(SHIFT_ENABLE, Signal::Level(false), Time(0), &mut drive);
        assert_eq!(
            adc.transfer(transfer(mode, 2, 0, 8), &mut drive),
            after,
            "D0 until the next falling edge, then D1 up to D7, mode {mode}"
        );
    }

    /// Volts in the middle of `code`'s step on 5 V.
    fn code_volts(code: u8) -> f64 {
        f64::from(code) * 5.0 / 256.0
    }

    #[test]
    fn an_undriven_input_reads_zero_volts() {
        let mut adc = adc();
        let inputs: Inputs = [None; CHANNELS + 1];
        assert_eq!(
            frame(&mut adc, inputs, &[(command(0b1000), 16)]),
            [reply(0)]
        );
    }

    #[test]
    fn the_parameters_are_checked() {
        assert!(built("vref_volts = 0.0", 0).is_err());
        assert!(built("vref_volts = -5", 0).is_err());
        assert!(built("noise_volts = -0.1", 0).is_err());
        assert!(built("vref_volts = \"high\"", 0).is_err());
        let peripheral = built("vref_volts = 2\nnoise_volts = 0.01", 0).unwrap();
        assert_eq!(
            peripheral.configuration,
            Configuration {
                vref_volts: 2.0,
                noise_volts: 0.01
            }
        );
    }

    /// Readings of channel 0 at 2.5 V with `noise_volts`, seeded from
    /// `seed`.
    fn noisy(noise_volts: f64, seed: u64) -> Vec<u16> {
        let mut adc = built(&format!("noise_volts = {noise_volts:?}"), seed).unwrap();
        let mut inputs: Inputs = [None; CHANNELS + 1];
        inputs[0] = Some(2.5);
        (0..64)
            .flat_map(|_| frame(&mut adc, inputs, &[(command(0b1000), 16)]))
            .collect()
    }

    #[test]
    fn noise_is_off_by_default_and_the_seeds_when_on() {
        assert!(noisy(0.0, 1).iter().all(|&reading| reading == reply(0x80)));
        let readings = noisy(0.02, 1);
        assert_eq!(readings, noisy(0.02, 1));
        assert_ne!(readings, noisy(0.02, 2));
        let codes: Vec<u16> = readings.iter().map(|reading| reading >> 2 & 0xff).collect();
        assert!(codes.iter().any(|&code| code != 0x80), "{codes:?}");
        assert!(
            codes.iter().all(|&code| code.abs_diff(0x80) <= 7),
            "{codes:?}"
        );
    }

    /// The rest of a frame of channel 2 begun with an 8-bit transfer,
    /// then eight more: the replies.
    fn rest(adc: &mut Adc0838, inputs: Inputs, drive: &mut Drive<'_>) -> (u16, Vec<u16>) {
        let tail = adc.transfer(transfer(3, 1, 0, 16), drive);
        adc.select(0, false, Time(0), drive);
        let next = (0..8)
            .flat_map(|_| frame(adc, inputs, &[(command(0b1001), 16)]))
            .collect();
        (tail, next)
    }

    #[test]
    fn a_restore_continues_the_frame_and_the_noise() {
        let mut inputs: Inputs = [None; CHANNELS + 1];
        inputs[2] = Some(1.7);
        let sampler = move |port: usize, _: Time| inputs.get(port).copied().flatten();
        let mut first = built("noise_volts = 0.05", 3).unwrap();
        let mut drive = Drive::with_sampler(Time(0), &sampler);
        for _ in 0..5 {
            frame(&mut first, inputs, &[(command(0b1001), 16)]);
        }
        first.select(0, true, Time(0), &mut drive);
        first.transfer(transfer(3, 0, 0b1_1001 << 3, 8), &mut drive);
        let snapshot = first.save().unwrap();
        let mut second = built("noise_volts = 0.05", 99).unwrap();
        second.restore(&snapshot).unwrap();
        let mut drive_second = Drive::with_sampler(Time(0), &sampler);
        assert_eq!(
            rest(&mut first, inputs, &mut drive),
            rest(&mut second, inputs, &mut drive_second)
        );
        let mut truncated = snapshot.clone();
        truncated.truncate(snapshot.len() - 1);
        let before = second.state.clone();
        assert!(second.restore(&truncated).is_err());
        assert!(second.restore(&[0xff; 4]).is_err());
        assert_eq!(second.state, before, "a refused snapshot changes nothing");
    }

    #[test]
    fn it_refuses_another_configuration_and_a_frame_past_its_end() {
        let mut adc = built("noise_volts = 0.05", 3).unwrap();
        frame(&mut adc, [None; CHANNELS + 1], &[(command(0b1001), 16)]);
        let before = adc.state.clone();
        for other in ["vref_volts = 4.096\nnoise_volts = 0.05", ""] {
            let snapshot = built(other, 3).unwrap().save().unwrap();
            assert!(matches!(
                adc.restore(&snapshot),
                Err(StateError::Mismatch(_))
            ));
        }
        let phases = [
            Phase::Addressing {
                address: 0,
                count: 4,
            },
            Phase::Addressing {
                address: 0b100,
                count: 2,
            },
            Phase::Output {
                result: 0,
                edges: 18,
            },
        ];
        for phase in phases {
            let state = State {
                phase,
                ..before.clone()
            };
            assert!(matches!(
                adc.restore(&encode(VERSION, &(adc.configuration, state))),
                Err(StateError::Invalid(_))
            ));
        }
        assert_eq!(adc.state, before);
    }

    #[test]
    fn the_format_matches_its_golden() {
        let mut adc = built("noise_volts = 0.01", 42).unwrap();
        adc.state.noise.normal();
        adc.state.phase = Phase::Output {
            result: 0xa5,
            edges: 3,
        };
        adc.state.shift_enable = true;
        snapshot_golden::check(
            env!("CARGO_MANIFEST_DIR"),
            NAME,
            &[(NAME, VERSION)],
            &adc.save().unwrap(),
        );
    }
}
