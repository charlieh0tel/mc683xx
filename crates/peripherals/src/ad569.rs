//! `ad569`: Analog Devices' AD569, a 16-bit voltage-output DAC with a
//! parallel input (AD569 datasheet, Rev. A).
//!
//! DB15-DB0, the bus input `db`, pass through a first rank of two byte
//! latches, the 8 MSBs and the 8 LSBs, into the 16-bit DAC register,
//! which VOUT, the analog output `out`, follows.  Four controls, the
//! logic inputs `cs`, `hbe`, `lbe` and `ldac`, gate them (Digital
//! Circuit Connections, Table I): with CS low, HBE low makes the MSB
//! latch transparent, LBE low the LSB latch, and LDAC low the DAC
//! register; with CS high nothing changes.  "All four control inputs
//! latches are level-triggered and active low": a latch follows its
//! input while enabled and holds from its enable's rise.  All four low
//! make every latch transparent, DB to VOUT.  Each control takes its
//! level from its parameter until a net drives it.  The latches power
//! on holding `initial`, since the datasheet gives no power-on state.

use mc683xx_board::inspect::Inspection;
use mc683xx_board::peripheral::Drive;
use mc683xx_board::peripheral::Peripheral;
use mc683xx_board::registry::Context;
use mc683xx_board::registry::ParameterError;
use mc683xx_board::registry::PortSpec;
use mc683xx_board::registry::Registry;
use mc683xx_board::registry::RegistryError;
use mc683xx_board::signal::Kind;
use mc683xx_board::signal::Signal;
use mc683xx_board::time::Time;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use serde::Deserialize;
use serde::Serialize;

use crate::dac::Span;
use crate::dac::Volts;
use crate::octal::LEVELS;
use crate::octal::level;

/// Its registered name.
pub(crate) const NAME: &str = "ad569";

/// The format of its snapshots.
const VERSION: u32 = 3;

/// Its resolution (Specifications, Resolution).
const CODE_BITS: u32 = 16;
/// A first-rank latch's bits: a byte of DB15-DB0.
const BYTE_BITS: u32 = 8;

/// Its ports, by index: DB15-DB0, the four controls, VOUT.
const DB: usize = 0;
const CS: usize = 1;
const HBE: usize = 2;
const LBE: usize = 3;
const LDAC: usize = 4;
const OUT: usize = 5;

/// The controls' names, as ports and as the parameters that tie them
/// until a net drives them, in port order from `CS`.  Each is high, not
/// enabled, by default.
const CONTROLS: [&str; 4] = ["cs", "hbe", "lbe", "ldac"];

/// The 16-bit word the MSB and LSB latches and the DAC register hold
/// out of power-on (the datasheet gives none: Product Description,
/// Digital Circuit Connections).
const INITIAL: &str = "initial";

/// The reference inputs, in volts: -VREF the output at code 0000H and
/// +VREF that a code past FFFFH would give (Analog Circuit
/// Connections: offset binary, "-VREF for a digital input code of all
/// zeros (0000H) to +VREF for an input code of all ones").  The
/// defaults are the specifications' conditions, +VREF +5 V and -VREF
/// -5 V (Specifications).
const VREF_NEGATIVE: (&str, Volts) = ("vref_negative", Volts(-5.0));
const VREF_POSITIVE: (&str, Volts) = ("vref_positive", Volts(5.0));

/// The levels on its four controls, high not enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Controls {
    /// CS.
    cs: bool,
    /// HBE: the 8 MSBs' latch.
    hbe: bool,
    /// LBE: the 8 LSBs' latch.
    lbe: bool,
    /// LDAC: the DAC register.
    ldac: bool,
}

/// What it holds, saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    /// The word on DB15-DB0, 0 until a net drives it.
    db: u16,
    /// The first rank: the 8 MSBs' latch.
    high: u8,
    /// The first rank: the 8 LSBs' latch.
    low: u8,
    /// The DAC register, which VOUT follows.
    dac: u16,
    /// The controls.
    controls: Controls,
}

impl State {
    /// Every latch the controls make transparent takes its input, in
    /// order down the path (Table I).
    fn follow(&mut self) {
        let Controls { cs, hbe, lbe, ldac } = self.controls;
        if cs {
            return;
        }
        if !hbe {
            // the high byte, by the shift
            self.high = (self.db >> BYTE_BITS) as u8;
        }
        if !lbe {
            // the low byte, by the cut
            self.low = self.db as u8;
        }
        if !ldac {
            self.dac = u16::from(self.high) << BYTE_BITS | u16::from(self.low);
        }
    }

    /// Refuses a latch its controls make transparent that does not
    /// hold its input: every change of DB or a control follows.
    fn check(self) -> Result<(), StateError> {
        let mut followed = self;
        followed.follow();
        if followed == self {
            Ok(())
        } else {
            Err(StateError::invalid(
                "an AD569 latch not following its input",
            ))
        }
    }
}

/// The peripheral.  No RESET reaches it: the AD569 has no reset input (its
/// four control inputs are CS, LBE, HBE and LDAC; Product Description),
/// so a reset changes nothing.  The datasheet gives no power-on state;
/// it powers on here with its latches holding `initial` (0 by default,
/// VOUT at -VREF), then follows whatever its tied controls make
/// transparent.
#[derive(Debug)]
struct Ad569 {
    /// VOUT's span: -VREF to +VREF.
    span: Span,
    /// What it holds.
    state: State,
}

impl Ad569 {
    /// From `initial` (0), `vref_negative`, `vref_positive` and the
    /// controls' levels.
    fn from_parameters(context: &Context<'_>) -> Result<Self, ParameterError> {
        let tied = |index: usize| level(context, CONTROLS[index], LEVELS, true);
        let initial: u16 = context.parameters.integer(INITIAL)?.unwrap_or(0);
        Ok(Self {
            span: Span::from_parameters(context, VREF_NEGATIVE, VREF_POSITIVE)?,
            state: State {
                db: 0,
                // the high byte, by the shift
                high: (initial >> BYTE_BITS) as u8,
                // the low byte, by the cut
                low: initial as u8,
                dac: initial,
                controls: Controls {
                    cs: tied(0)?,
                    hbe: tied(1)?,
                    lbe: tied(2)?,
                    ldac: tied(3)?,
                },
            },
        })
    }

    /// Its ports, by index.
    fn ports() -> Vec<PortSpec> {
        let controls = CONTROLS
            .iter()
            .map(|name| PortSpec::input(*name, Kind::Logic));
        [PortSpec::input("db", Kind::Bus)]
            .into_iter()
            .chain(controls)
            .chain([PortSpec::output("out", Kind::Analog)])
            .collect()
    }

    /// Drives VOUT.
    fn drive_output(&self, drive: &mut Drive<'_>) {
        let Volts(volts) = self.span.output(self.state.dac, CODE_BITS);
        drive.drive(OUT, Signal::Analog(volts));
    }
}

impl Peripheral for Ad569 {
    fn power_on(&mut self, drive: &mut Drive<'_>) {
        self.state.follow();
        self.drive_output(drive);
    }

    fn input(&mut self, port: usize, signal: Signal, _: Time, drive: &mut Drive<'_>) {
        let controls = &mut self.state.controls;
        match (port, signal) {
            // DB15-DB0, by the cut
            (DB, Signal::Bus(word)) => self.state.db = word as u16,
            (CS, Signal::Level(level)) => controls.cs = level,
            (HBE, Signal::Level(level)) => controls.hbe = level,
            (LBE, Signal::Level(level)) => controls.lbe = level,
            (LDAC, Signal::Level(level)) => controls.ldac = level,
            _ => return,
        }
        let before = self.state.dac;
        self.state.follow();
        if self.state.dac != before {
            self.drive_output(drive);
        }
    }

    /// The latches, VOUT and the controls' levels.
    fn inspect(&self) -> Inspection {
        let State {
            db,
            high,
            low,
            dac,
            controls,
        } = self.state;
        let Volts(volts) = self.span.output(dac, CODE_BITS);
        Inspection::default()
            .register("db", db)
            .register("high", high)
            .register("low", low)
            .register("dac", dac)
            .real("out", volts, "V")
            .group(
                "controls",
                Inspection::default()
                    .boolean("cs", controls.cs)
                    .boolean("hbe", controls.hbe)
                    .boolean("lbe", controls.lbe)
                    .boolean("ldac", controls.ldac),
            )
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(VERSION, &(self.span, self.state)))
    }

    /// Refuses another span's snapshot and one `State::check` refuses.
    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        let (span, state): (Span, State) = decode(VERSION, snapshot)?;
        if span != self.span {
            return Err(StateError::mismatch("an AD569 of other references"));
        }
        state.check()?;
        self.state = state;
        Ok(())
    }
}

/// Registers `ad569`.
pub(crate) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register(NAME, Ad569::ports(), |context| {
        Ok(Box::new(Ad569::from_parameters(context)?))
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

    use super::Ad569;
    use super::CS;
    use super::DB;
    use super::HBE;
    use super::LBE;
    use super::LDAC;
    use super::OUT;
    use super::State;
    use super::VERSION;

    /// A peripheral from the parameters `table`, in TOML.
    fn peripheral(table: &str) -> Result<Ad569, ParameterError> {
        let parameters = Parameters::parse(table).unwrap();
        let environment = Environment::default();
        let built = Ad569::from_parameters(&Context::new("dac", &parameters, &environment));
        assert_eq!(parameters.unread(), None);
        built
    }

    /// What `signal` on `port` drove on VOUT.
    fn input(dac: &mut Ad569, port: usize, signal: Signal) -> Vec<f64> {
        let mut drive = Drive::new(Time::ZERO);
        dac.input(port, signal, Time::ZERO, &mut drive);
        drive
            .finish()
            .driven
            .iter()
            .map(|driven| match (driven.port, driven.signal) {
                (OUT, Signal::Analog(volts)) => volts,
                other => panic!("not VOUT: {other:?}"),
            })
            .collect()
    }

    /// `port` low then high: a strobe.
    fn strobe(dac: &mut Ad569, port: usize) -> Vec<f64> {
        let mut driven = input(dac, port, Signal::Level(false));
        driven.extend(input(dac, port, Signal::Level(true)));
        driven
    }

    /// VOUT for `code` at the default references.
    fn volts(code: u16) -> f64 {
        -5.0 + 10.0 * f64::from(code) / 65536.0
    }

    #[test]
    fn three_strobes_load_the_bytes_then_the_dac() {
        // an 8-bit bus: HBE, LBE, then LDAC, CS tied low (Figure 20)
        let mut dac = peripheral("cs = \"low\"").unwrap();
        input(&mut dac, DB, Signal::Bus(0xc0c0));
        assert!(strobe(&mut dac, HBE).is_empty());
        input(&mut dac, DB, Signal::Bus(0xa0a0));
        assert!(strobe(&mut dac, LBE).is_empty());
        input(&mut dac, DB, Signal::Bus(0));
        assert_eq!(strobe(&mut dac, LDAC), [volts(0xc0a0)]);
    }

    #[test]
    fn cs_high_is_no_operation() {
        let mut dac = peripheral("hbe = \"low\"\nlbe = \"low\"\nldac = \"low\"").unwrap();
        assert!(input(&mut dac, DB, Signal::Bus(0x1234)).is_empty());
        assert_eq!(input(&mut dac, CS, Signal::Level(false)), [volts(0x1234)]);
        input(&mut dac, CS, Signal::Level(true));
        assert!(input(&mut dac, DB, Signal::Bus(0x5678)).is_empty());
        assert!(strobe(&mut dac, LDAC).is_empty(), "CS gates LDAC");
    }

    #[test]
    fn all_low_is_transparent() {
        let mut dac =
            peripheral("cs = \"low\"\nhbe = \"low\"\nlbe = \"low\"\nldac = \"low\"").unwrap();
        assert_eq!(input(&mut dac, DB, Signal::Bus(0x8000)), [0.0]);
        assert!(
            input(&mut dac, DB, Signal::Bus(0x1_8000)).is_empty(),
            "DB15-DB0 only"
        );
    }

    #[test]
    fn with_hbe_and_ldac_enabled_lbe_completes_the_word() {
        // CS, HBE and LDAC tied low: the MSBs reach VOUT as DB
        // changes, the LSBs at LBE
        let mut dac = peripheral("cs = \"low\"\nhbe = \"low\"\nldac = \"low\"").unwrap();
        assert_eq!(input(&mut dac, DB, Signal::Bus(0xc0a0)), [volts(0xc000)]);
        assert_eq!(strobe(&mut dac, LBE), [volts(0xc0a0)]);
    }

    #[test]
    fn the_references_set_the_span() {
        let mut dac = peripheral(
            "vref_negative = 0\nvref_positive = -8.0\ncs = \"low\"\n\
             hbe = \"low\"\nlbe = \"low\"\nldac = \"low\"",
        )
        .unwrap();
        assert_eq!(input(&mut dac, DB, Signal::Bus(0x4000)), [-2.0]);
        assert!(matches!(
            peripheral("vref_positive = inf"),
            Err(ParameterError::Invalid { .. })
        ));
        assert!(matches!(
            peripheral("ldac = \"active\""),
            Err(ParameterError::Invalid { .. })
        ));
    }

    #[test]
    fn power_on_drives_vout_at_minus_vref() {
        let mut dac = peripheral("").unwrap();
        let mut drive = Drive::new(Time::ZERO);
        dac.power_on(&mut drive);
        assert_eq!(drive.finish().driven[0].signal, Signal::Analog(-5.0));
    }

    #[test]
    fn initial_loads_the_latches_and_the_dac_register() {
        let mut dac = peripheral("initial = 0x8000").unwrap();
        assert_eq!(
            (dac.state.high, dac.state.low, dac.state.dac),
            (0x80, 0, 0x8000)
        );
        let mut drive = Drive::new(Time::ZERO);
        dac.power_on(&mut drive);
        assert_eq!(drive.finish().driven[0].signal, Signal::Analog(0.0));
        // LDAC alone takes the latches' word, not DB's
        let mut dac = peripheral("initial = 0xc0a0\ncs = \"low\"").unwrap();
        dac.power_on(&mut Drive::new(Time::ZERO));
        assert!(strobe(&mut dac, LDAC).is_empty());
        assert!(strobe(&mut dac, HBE).is_empty());
        assert_eq!(strobe(&mut dac, LDAC), [volts(0x00a0)]);
        assert!(matches!(
            peripheral("initial = 0x10000"),
            Err(ParameterError::Range { .. })
        ));
    }

    #[test]
    fn a_snapshot_restores_into_another_initial() {
        let dac = peripheral("initial = 0x8000").unwrap();
        let mut other = peripheral("").unwrap();
        other.restore(&dac.save().unwrap()).unwrap();
        assert_eq!(other.state, dac.state);
    }

    #[test]
    fn it_restores_what_it_saved() {
        let mut dac = peripheral("cs = \"low\"\nhbe = \"low\"").unwrap();
        input(&mut dac, DB, Signal::Bus(0x1234));
        strobe(&mut dac, LBE);
        let snapshot = dac.save().unwrap();
        let mut other = peripheral("").unwrap();
        other.restore(&snapshot).unwrap();
        assert_eq!(other.state, dac.state);
        assert!(other.restore(&[]).is_err());
    }

    #[test]
    fn it_refuses_other_references_and_a_latch_not_following() {
        let mut dac = peripheral("cs = \"low\"\nhbe = \"low\"").unwrap();
        input(&mut dac, DB, Signal::Bus(0x1234));
        let before = dac.state;
        let other = peripheral("vref_positive = 10.0").unwrap();
        assert!(matches!(
            dac.restore(&other.save().unwrap()),
            Err(StateError::Mismatch(_))
        ));
        let stale = State {
            high: 0x56,
            ..before
        };
        assert!(matches!(
            dac.restore(&encode(VERSION, &(dac.span, stale))),
            Err(StateError::Invalid(_))
        ));
        assert_eq!(dac.state, before);
    }

    #[test]
    fn it_shows_its_latches_and_vout() {
        let mut dac = peripheral("cs = \"low\"").unwrap();
        input(&mut dac, DB, Signal::Bus(0xc0a0));
        strobe(&mut dac, HBE);
        strobe(&mut dac, LBE);
        strobe(&mut dac, LDAC);
        let inspection = dac.inspect();
        assert_eq!(inspection.get("dac"), Some(&Value::Integer(0xc0a0)));
        assert_eq!(inspection.get("high"), Some(&Value::Integer(0xc0)));
        assert_eq!(inspection.get("out"), Some(&Value::Real(volts(0xc0a0))));
        let Some(Value::Group(controls)) = inspection.get("controls") else {
            panic!("the controls");
        };
        assert_eq!(controls.get("cs"), Some(&Value::Boolean(false)));
    }

    #[test]
    fn the_format_matches_its_golden() {
        let mut dac = peripheral("cs = \"low\"\nhbe = \"low\"").unwrap();
        input(&mut dac, DB, Signal::Bus(0x8123));
        strobe(&mut dac, LBE);
        strobe(&mut dac, LDAC);
        input(&mut dac, DB, Signal::Bus(0x5555));
        snapshot_golden::check(
            env!("CARGO_MANIFEST_DIR"),
            "ad569",
            &[("ad569", VERSION)],
            &dac.save().unwrap(),
        );
    }
}
