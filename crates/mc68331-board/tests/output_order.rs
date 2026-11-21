//! The chip's outputs reach the nets in the order they happened: a PWM
//! pin and a port GP pin the firmware changes in one instruction.

use std::env;
use std::fs;
use std::process;

use mc683xx_board::board::Board;
use mc683xx_board::description::Description;
use mc683xx_board::description::Options;
use mc683xx_board::peripheral::Drive;
use mc683xx_board::peripheral::Peripheral;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::PortSpec;
use mc683xx_board::registry::Registry;
use mc683xx_board::signal::Kind;
use mc683xx_board::signal::Signal;
use mc683xx_board::time::Time;
use mc683xx_mc68331::chip::Mc68331;

/// The probe's clock port.
const CLOCK: usize = 0;
/// The probe's data port.
const DATA: usize = 1;
/// Where the program starts.
const START: u32 = 0x400;
/// The initial stack pointer.
const STACK: u32 = 0x10_fffe;
/// The ROM's size.
const ROM_BYTES: usize = 0x800;
/// DDRGP in the upper byte, PORTGP in the lower (MC68331UM Table D-2).
const DDRGP_PORTGP: u32 = 0xff_f906;
/// CFORC in the upper byte, PWMC in the lower.
const CFORC_PWMC: u32 = 0xff_f924;
/// FPWMA with F1A: PWMA forced high (GPTRM 6.3.1, 3.3.4).
const PWMA_HIGH: u16 = 0x0202;
/// FPWMA without F1A: PWMA forced low.
const PWMA_LOW: u16 = 0x0200;
/// MOVE.W #imm,(xxx).L.
const MOVE_WORD_TO_ABSOLUTE: u16 = 0x33fc;
/// MOVEA.L #imm,A0; A1's is 0x0200 more.
const MOVEA_LONG_TO_A0: u16 = 0x207c;
/// MOVEA.L #imm,A1.
const MOVEA_LONG_TO_A1: u16 = 0x227c;
/// MOVE.L #imm,D0.
const MOVE_LONG_TO_D0: u16 = 0x203c;
/// MOVEM.L list,-(A1).
const MOVEM_LONG_TO_PREDECREMENT_A1: u16 = 0x48e1;
/// D0-D7/A0 as a predecrement mask: D0 in bit 15 (CPU32RM 4, MOVEM).
const D0_D7_A0: u16 = 0xff80;
/// BRA.S to itself.
const BRANCH_TO_SELF: u16 = 0x60fe;

/// A flip-flop: the data level it saw at each rising clock edge.
#[derive(Debug, Default)]
struct Probe {
    /// The clock's level.
    clock: bool,
    /// The data's level, once it has one.
    data: Option<bool>,
    /// The data at each rising clock edge.
    sampled: Vec<Option<bool>>,
}

impl Peripheral for Probe {
    fn input(&mut self, port: usize, signal: Signal, _drive: &mut Drive<'_>) {
        let Signal::Level(level) = signal else {
            return;
        };
        match port {
            CLOCK => {
                if level && !self.clock {
                    self.sampled.push(self.data);
                }
                self.clock = level;
            }
            DATA => self.data = Some(level),
            _ => {}
        }
    }
}

/// MOVE.W #`value`,`address`.
fn store(address: u32, value: u16) -> [u16; 4] {
    [
        MOVE_WORD_TO_ABSOLUTE,
        value,
        (address >> 16) as u16,
        address as u16,
    ]
}

/// MOVE.L or MOVEA.L #`value` by `opcode`.
fn load(opcode: u16, value: u32) -> [u16; 3] {
    [opcode, (value >> 16) as u16, value as u16]
}

/// The reviewer's firmware: PGP0 an output, low then high; a PWMA pulse;
/// then one MOVEM that forces PWMA high (A0 to CFORC and PWMC) and drives
/// PGP0 low (D0 to DDRGP and PORTGP), in that order.
fn program() -> Vec<u16> {
    [
        &store(DDRGP_PORTGP, 0x0100)[..],
        &store(DDRGP_PORTGP, 0x0101),
        &store(CFORC_PWMC, PWMA_HIGH),
        &store(CFORC_PWMC, PWMA_LOW),
        &load(MOVEA_LONG_TO_A0, 0x0202_0000),
        &load(MOVEA_LONG_TO_A1, CFORC_PWMC + 4),
        &load(MOVE_LONG_TO_D0, 0x0000_0100),
        &[MOVEM_LONG_TO_PREDECREMENT_A1, D0_D7_A0, BRANCH_TO_SELF],
    ]
    .concat()
}

/// A ROM image with the reset vectors and `program` at `START`.
fn image(program: &[u16]) -> Vec<u8> {
    let mut image = vec![0; ROM_BYTES];
    image[0..4].copy_from_slice(&STACK.to_be_bytes());
    image[4..8].copy_from_slice(&START.to_be_bytes());
    for (index, word) in program.iter().enumerate() {
        let at = START as usize + 2 * index;
        image[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
    image
}

#[test]
fn a_pwm_clock_and_port_gp_data_changed_in_one_instruction_arrive_in_order() {
    let directory = env::temp_dir().join(format!("mc683xx-output-order-{}", process::id()));
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("rom.bin"), image(&program())).unwrap();
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "probe"
type = "probe"
[[net]]
name = "clock"
type = "logic"
from = "chip.PWMA"
to = ["probe.clock"]
[[net]]
name = "data"
type = "logic"
from = "chip.PGP0"
to = ["probe.data"]
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register(
            "probe",
            vec![
                PortSpec::input("clock", Kind::Logic),
                PortSpec::input("data", Kind::Logic),
            ],
            |_| Ok(Box::new(Probe::default())),
        )
        .unwrap();
    let mut board =
        Board::<Mc68331>::load(&description, &registry, &Environment::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    board.run_until(Time::from_picos(100_000_000)).unwrap();
    let sampled = board
        .peripheral::<Probe>("probe")
        .map(|probe| probe.sampled.clone());
    // the MOVEM's rising edge comes before its fall on PGP0
    assert_eq!(sampled, Some(vec![Some(true), Some(true)]));
}
