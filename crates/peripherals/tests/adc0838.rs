//! An ADC0838 on a board: a fixed analog source on a channel, and CPU
//! code that converts it over the QSPI and reads the reply; and the
//! same beside a 74x164 that sees every transfer.

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

/// The initial stack pointer and program counter in the image.
const STACK: u32 = 0x10_fffe;
/// Where the program starts.
const START: u32 = 0x400;
/// The boot ROM's size.
const ROM_BYTES: usize = 0x1_0000;
/// The QSM's registers with MM set (MC68331UM Table D-1).
const QSM: u32 = 0xff_fc00;
/// PORTQS, the lower byte (QSMRM 3.5): PCS[3:0] high while not the
/// QSPI's.
const PORTQS: u32 = QSM + 0x14;
/// PQSPAR and DDRQS (QSMRM 3.5): MISO, MOSI and PCS[3:0] the QSPI's;
/// MOSI, SCK and PCS[3:0] outputs.
const PQSPAR_DDRQS: u32 = QSM + 0x16;
/// SPCR0 (QSMRM 4.3.1).
const SPCR0: u32 = QSM + 0x18;
/// SPCR1 (QSMRM 4.3.2).
const SPCR1: u32 = QSM + 0x1a;
/// SPCR2 (QSMRM 4.3.3).
const SPCR2: u32 = QSM + 0x1c;
/// Receive RAM entry 0 (QSMRM 4.3.6.1).
const RECEIVE_RAM: u32 = QSM + 0x100;
/// Transmit RAM entry 0 (QSMRM 4.3.6.2).
const TRANSMIT_RAM: u32 = QSM + 0x120;
/// Command RAM entries 0 and 1, a byte each (QSMRM 4.3.6.3).
const COMMAND_RAM: u32 = QSM + 0x140;
/// PCS[3:0] high, the other port QS pins low.
const PCS_HIGH: u16 = 0x0078;
/// PQSPAR $7B, DDRQS $7E.
const QSPI_PINS: u16 = 0x7b7e;
/// SPCR0: master, 16 bits, CPOL and CPHA clear, SPBR 16.
const MASTER_16_BITS: u16 = 0x8010;
/// SPCR1: SPE.
const ENABLE: u16 = 0x8000;
/// SPCR2: ENDQP and NEWQP 0, a queue of entry 0 alone.
const ONE_ENTRY: u16 = 0x0000;
/// Entry 0's command: BITSE, SPCR0's 16 bits; PCS 1110, the ADC's
/// select low; no CONT, so PCS returns high after it.
const ENTRY_TO_ADC: u16 = 0x4e00;
/// The PCS levels that select the ADC.
const ADC_SELECT: u8 = 0b1110;
/// The start bit, then single-ended channel 3: SGL/DIF 1, ODD/SIGN 1,
/// SELECT 01 (SNAS531B, Table 2), at the top of the word.
const CONVERT_CHANNEL_3: u16 = 0b1_1101 << 11;
/// MOVE.W #imm,(xxx).L (CPU32RM 4, MOVE).
const MOVE_WORD_TO_ABSOLUTE: u16 = 0x33fc;
/// MOVE.W (xxx).L,D0.
const MOVE_WORD_TO_D0: u16 = 0x3039;
/// BRA.S back over the six words before it.
const BRA_BACK_SIX: u16 = 0x60f2;
/// NOP.
const NOP: u16 = 0x4e71;

/// The source's level and the ADC's reference.
const SOURCE_VOLTS: f64 = 1.234;
/// The ADC's reference.
const VREF_VOLTS: f64 = 2.5;

/// MOVE.W #`value`,`address`.
fn store(address: u32, value: u16) -> [u16; 4] {
    [
        MOVE_WORD_TO_ABSOLUTE,
        value,
        (address >> 16) as u16,
        address as u16,
    ]
}

/// The program: start a one-entry queue sending `word`, then read
/// receive RAM entry 0 into D0 for ever.
fn program(word: u16) -> Vec<u16> {
    [
        store(PORTQS, PCS_HIGH),
        store(PQSPAR_DDRQS, QSPI_PINS),
        store(TRANSMIT_RAM, word),
        store(COMMAND_RAM, ENTRY_TO_ADC),
        store(SPCR2, ONE_ENTRY),
        store(SPCR0, MASTER_16_BITS),
        store(SPCR1, ENABLE),
    ]
    .concat()
    .into_iter()
    .chain([
        MOVE_WORD_TO_D0,
        (RECEIVE_RAM >> 16) as u16,
        RECEIVE_RAM as u16,
        NOP,
        NOP,
        NOP,
        BRA_BACK_SIX,
    ])
    .collect()
}

/// A ROM image with the reset vectors and `program` at `START`.
fn image(program: &[u16]) -> Vec<u8> {
    let mut image: Vec<u8> = NOP.to_be_bytes().repeat(ROM_BYTES / 2);
    image[0..4].copy_from_slice(&STACK.to_be_bytes());
    image[4..8].copy_from_slice(&START.to_be_bytes());
    for (index, word) in program.iter().enumerate() {
        let at = START as usize + 2 * index;
        image[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
    image
}

/// A fixed analog source: drives its level from power-on.
#[derive(Debug)]
struct Source(f64);

impl Peripheral for Source {
    fn power_on(&mut self, drive: &mut Drive<'_>) {
        drive.drive(0, Signal::Analog(self.0));
    }
}

/// The board: the ROM, the source on the ADC's channel 3, and `more`;
/// run with `program(word)` until the reply is in D0.
fn run(word: u16, more: &str) -> Board<Mc68331> {
    let directory = env::temp_dir().join(format!(
        "mc683xx-peripherals-adc0838-{}-{word:04x}-{}",
        process::id(),
        more.len()
    ));
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("rom.bin"), image(&program(word))).unwrap();
    let text = format!(
        r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{{ select = "CSBOOT" }}]
[[peripheral]]
name = "source"
type = "source"
[[peripheral]]
name = "adc"
type = "adc0838"
parameters = {{ vref_volts = {VREF_VOLTS:?} }}
attach = [{{ qspi = {ADC_SELECT:#06b} }}]
[[net]]
name = "sensor"
type = "analog"
from = "source.out"
to = ["adc.ch3"]
{more}"#
    );
    let description = Description::parse(&text, &directory, &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register(
            "source",
            vec![PortSpec::output("out", Kind::Analog)],
            |_| Ok(Box::new(Source(SOURCE_VOLTS))),
        )
        .unwrap();
    mc683xx_peripherals::register(&mut registry).unwrap();
    let mut board =
        Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    board.run_until(Time(10_000_000_000)).unwrap();
    board
}

/// The ADC's reply to `CONVERT_CHANNEL_3`: 1.234 V on a 2.5 V
/// reference, 126.4 LSBs, code 126; five clocks in TRI-STATE (ones),
/// the leading zero, D7 to D0, then LSB first D1 and D2 (SNAS531B,
/// Figure 21).
const REPLY: u16 = {
    let code: u16 = 126;
    0b1_1111 << 11 | code << 2 | (code >> 1 & 1) << 1 | (code >> 2 & 1)
};

/// D0's low word.
fn d0(board: &Board<Mc68331>) -> u16 {
    board.chip().cpu().registers.data[0] as u16
}

#[test]
fn cpu_code_reads_a_channel_over_the_qspi() {
    let board = run(CONVERT_CHANNEL_3, "");
    assert_eq!(d0(&board), REPLY, "{:#06x}", d0(&board));
}

/// A 74x164 on the QSPI ungated, `mask = 0`, with `miso`, its outputs
/// to `external.q`.
fn ungated_164(miso: &str) -> String {
    format!(
        r#"
[[peripheral]]
name = "sipo"
type = "74x164"
attach = [{{ qspi = 0, mask = 0 }}]
parameters = {{ miso = "{miso}" }}
[[net]]
name = "q"
type = "bus"
from = "sipo.Q"
to = ["external.q"]
"#
    )
}

#[test]
fn every_selected_peripheral_sees_the_transfer_and_miso_is_their_and() {
    // the ADC ignores DI after its address (SNAS531B, THE DIGITAL
    // INTERFACE step 4), so the low byte is the '164's alone
    let word = CONVERT_CHANNEL_3 | 0x5a;
    // QH on MISO: it drives the old stages, then the word's high byte,
    // and its zeros win over the ADC's reply
    let mut board = run(word, &ungated_164("qh"));
    assert_eq!(d0(&board), REPLY & word >> 8);
    let q = board.take_received("q").unwrap();
    assert_eq!(q.last().map(|(_, signal)| *signal), Some(Signal::Bus(0x5a)));
    // MISO not wired to QH: the ADC's reply survives
    let mut board = run(word, &ungated_164("none"));
    assert_eq!(d0(&board), REPLY);
    let q = board.take_received("q").unwrap();
    assert_eq!(q.last().map(|(_, signal)| *signal), Some(Signal::Bus(0x5a)));
}
