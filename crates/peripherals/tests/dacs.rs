//! The DACs on a board: CPU code fills a QSPI queue for the MC144110
//! and another for a 16-bit 74x164 chain whose outputs are the AD569's
//! DB15-DB0, then strobes the AD569's LBE from a port GP pin; their
//! outputs reach the nets in volts.  And smartclock's hex DAC self-test:
//! the MC144110 written in SPI mode 2 and read back in mode 3.

use std::env;
use std::fs;
use std::process;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use mc683xx_board::board::Board;

use mc683xx_board::description::Description;
use mc683xx_board::description::Options;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::Registry;
use mc683xx_board::signal::Signal;
use mc683xx_board::time::Time;
use mc683xx_mc68331::chip::Mc68331;

/// The initial stack pointer and program counter in the image.
const STACK: u32 = 0x10_fffe;
const START: u32 = 0x400;
/// The boot ROM's size.
const ROM_BYTES: usize = 0x1_0000;
/// The QSM's registers with MM set (MC68331UM Table D-1).
const QSM: u32 = 0xff_fc00;
/// PORTQS's word: PORTQS in its low byte (QSMRM 3.3.1).
const PORTQS_WORD: u32 = QSM + 0x14;
/// PQSPAR and DDRQS (QSMRM 3.3.2, 3.3.3).
const PQSPAR: u32 = QSM + 0x16;
/// SPCR0 to SPCR2, and SPCR3 with SPSR (QSMRM 4.3.1 to 4.3.5).
const SPCR0: u32 = QSM + 0x18;
const SPCR1: u32 = QSM + 0x1a;
const SPCR2: u32 = QSM + 0x1c;
const SPCR3: u32 = QSM + 0x1e;
/// SPSR, SPIF its bit 7.
const SPSR: u32 = QSM + 0x1f;
/// The receive, transmit and command RAMs (QSMRM 4.3.6, Figure 4-2).
const RECEIVE_RAM: u32 = QSM + 0x100;
const TRANSMIT_RAM: u32 = QSM + 0x120;
const COMMAND_RAM: u32 = QSM + 0x140;
/// The GPT's PDDR and PDR, one word (GPTRM 8.1, Figure 8-1).
const PDDR: u32 = 0xff_f906;

/// PCS[3:0] high when idle, PCS0-PCS3 the QSPI's outputs (QSMRM 3.3).
const PCS_IDLE: u16 = 0x0078;
const PCS_OUTPUTS: u16 = 0x7878;
/// A command: CONT, BITSE, and the PCS levels (QSMRM 4.3.6.3).
const CONT: u8 = 0x80;
const BITSE: u8 = 0x40;
/// The hex DAC's PCS code and the 74x164 chain's.
const HEX_DAC: u8 = 0b1110;
const CHAIN: u8 = 0b1101;
/// SPCR0: MSTR, BITS 12 or 16 (0), SPBR 2 (QSMRM 4.3.1).
const MASTER_12: u16 = 0xb002;
const MASTER_16: u16 = 0x8002;
/// SPCR1: SPE.
const SPE: u16 = 0x8000;
/// SPCR2: ENDQP 2, NEWQP 0; then ENDQP 4, NEWQP 3.
const ENTRIES_0_TO_2: u16 = 0x0200;
const ENTRIES_3_TO_4: u16 = 0x0403;
/// PDDR: PGP0 an output; PDR: its level.
const PGP0_HIGH: u16 = 0x0101;
const PGP0_LOW: u16 = 0x0100;

/// MOVE.W #imm,(xxx).L (CPU32RM 4, MOVE).
const MOVE_WORD_TO_ABSOLUTE: u16 = 0x33fc;
/// MOVE.W (xxx).L,D0.
const MOVE_WORD_TO_D0: u16 = 0x3039;
/// TST.B (xxx).L.
const TST_BYTE_ABSOLUTE: u16 = 0x4a39;
/// BPL.S back to the TST.B before it.
const BPL_BACK_TO_TST: u16 = 0x6af8;
/// BRA.S to itself.
const BRA_SELF: u16 = 0x60fe;
/// NOP.
const NOP: u16 = 0x4e71;

/// The hex DAC's codes, DAC 1 first, and the AD569's word.
const CODES: [u16; 6] = [1, 10, 20, 40, 50, 63];
const WORD: u16 = 0xc0a0;

/// MOVE.W #`value`,`address`.
fn store(address: u32, value: u16) -> [u16; 4] {
    [
        MOVE_WORD_TO_ABSOLUTE,
        value,
        (address >> 16) as u16,
        address as u16,
    ]
}

/// Waits for SPIF, then clears it.
fn await_spif() -> Vec<u16> {
    let mut code = vec![TST_BYTE_ABSOLUTE, (SPSR >> 16) as u16, SPSR as u16];
    code.push(BPL_BACK_TO_TST);
    code.extend(store(SPCR3, 0));
    code
}

/// The program: the hex DAC's six codes in three 12-bit entries, DAC 6
/// first, PCS held across them (CONT); then the AD569's word to the
/// chain twice, in two 16-bit entries, the second reading the first
/// back into the receive RAM; then LBE low and high on PGP0; then the
/// word read back into D0.
fn program() -> Vec<u16> {
    let pair = |high: u16, low: u16| high << 6 | low;
    let commands = [
        CONT | BITSE | HEX_DAC,
        CONT | BITSE | HEX_DAC,
        BITSE | HEX_DAC,
        BITSE | CHAIN,
        BITSE | CHAIN,
        0,
    ];
    let mut program = Vec::new();
    program.extend(store(PDDR, PGP0_HIGH));
    program.extend(store(PORTQS_WORD, PCS_IDLE));
    program.extend(store(PQSPAR, PCS_OUTPUTS));
    for (entry, word) in [
        pair(CODES[5], CODES[4]),
        pair(CODES[3], CODES[2]),
        pair(CODES[1], CODES[0]),
        WORD,
        WORD,
    ]
    .into_iter()
    .enumerate()
    {
        program.extend(store(TRANSMIT_RAM + 2 * entry as u32, word));
    }
    for (pair, commands) in commands.chunks(2).enumerate() {
        let word = u16::from(commands[0]) << 8 | u16::from(commands[1]);
        program.extend(store(COMMAND_RAM + 2 * pair as u32, word));
    }
    program.extend(store(SPCR0, MASTER_12));
    program.extend(store(SPCR2, ENTRIES_0_TO_2));
    program.extend(store(SPCR1, SPE));
    program.extend(await_spif());
    program.extend(store(SPCR0, MASTER_16));
    program.extend(store(SPCR2, ENTRIES_3_TO_4));
    program.extend(store(SPCR1, SPE));
    program.extend(await_spif());
    program.extend(store(PDDR, PGP0_LOW));
    program.extend(store(PDDR, PGP0_HIGH));
    let read_back = RECEIVE_RAM + 2 * 4;
    program.extend([MOVE_WORD_TO_D0, (read_back >> 16) as u16, read_back as u16]);
    program.push(BRA_SELF);
    program
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

/// The board: the ROM, the two DACs, their outputs to external
/// endpoints.
const BOARD: &str = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "hex"
type = "mc144110"
marking = "U12, MC144110P"
attach = [{ qspi = 0b1110 }]
[[peripheral]]
name = "chain"
type = "74x164"
attach = [{ qspi = 0b1101 }]
parameters = { cascade = 2 }
[[peripheral]]
name = "efc"
type = "ad569"
parameters = { cs = "low", hbe = "low", ldac = "low" }
[[net]]
name = "db"
type = "bus"
from = "chain.Q"
to = ["efc.db"]
[[net]]
name = "lbe"
type = "logic"
from = "chip.PGP0"
to = ["efc.lbe"]
[[net]]
name = "efc-out"
type = "analog"
from = "efc.out"
to = ["external.efc"]
"#;

/// The board with its ROM.
fn board() -> Board<Mc68331> {
    board_from(BOARD)
}

/// The board `text` describes, with its ROM.
fn board_from(text: &str) -> Board<Mc68331> {
    // one directory a board, for tests that run at once
    static BOARDS: AtomicUsize = AtomicUsize::new(0);
    let directory = env::temp_dir().join(format!(
        "mc683xx-peripherals-dacs-{}-{}",
        process::id(),
        BOARDS.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("rom.bin"), image(&program())).unwrap();
    let nets: String = (0..6)
        .map(|channel| {
            format!(
                "[[net]]\nname = \"hex-{channel}\"\ntype = \"analog\"\n\
                 from = \"hex.out{channel}\"\nto = [\"external.hex{channel}\"]\n"
            )
        })
        .collect();
    let text = format!("{text}{nets}");
    let description = Description::parse(&text, &directory, &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    mc683xx_peripherals::register(&mut registry).unwrap();
    let board = Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    board
}

/// The volts an analog value carries.
fn volts(signal: Signal) -> f64 {
    match signal {
        Signal::Analog(volts) => volts,
        other => panic!("not analog: {other:?}"),
    }
}

/// VOUT for `code` at the default references.
fn vout(code: u16) -> f64 {
    -5.0 + 10.0 * f64::from(code) / 65536.0
}

#[test]
fn a_chain_powering_on_mid_scale_starts_the_ad569_at_mid_scale() {
    // the transparent MSB latch and DAC register follow DB from power
    // on (Table I), so the chain's own power-on stages, not the
    // AD569's `initial`, are what VOUT starts at; LBE high holds the
    // LSB latch at the AD569's `initial` (0)
    let text = BOARD.replace(
        "parameters = { cascade = 2 }",
        "parameters = { cascade = 2, initial = 0x8000 }",
    );
    let mut board = board_from(&text);
    board.run_until(Time(1_000_000)).unwrap();
    let efc = board.take_received("efc").unwrap();
    let at_power_on: Vec<_> = efc.iter().filter(|(at, _)| *at == Time::ZERO).collect();
    assert_eq!(
        at_power_on.last().map(|(_, signal)| volts(*signal)),
        Some(0.0),
        "{efc:?}"
    );
}

#[test]
fn queues_set_the_dacs_and_lbe_completes_the_ad569s_word() {
    let mut board = board();
    assert_eq!(board.marking("hex"), Some("U12, MC144110P"));
    assert_eq!(board.marking("chain"), None);
    board.run_until(Time(10_000_000_000)).unwrap();
    let mut latched = Time::ZERO;
    for (channel, code) in CODES.into_iter().enumerate() {
        let received = board.take_received(&format!("hex{channel}")).unwrap();
        assert_eq!(received.len(), 2, "power-on and the frame: {received:?}");
        assert_eq!(received[0], (Time::ZERO, Signal::Analog(0.0)));
        assert_eq!(volts(received[1].1), 5.0 * f64::from(code) / 64.0);
        latched = received[1].0;
    }
    // CS, HBE and LDAC tied low (Table I): the MSB latch and the DAC
    // register follow DB, so the first transfer's MSBs reach VOUT as
    // it ends, the second (the same word) changes nothing, and LBE's
    // fall latches the LSBs
    let efc = board.take_received("efc").unwrap();
    assert_eq!(efc.len(), 3, "power-on, the MSBs, LBE: {efc:?}");
    assert_eq!(efc[0], (Time::ZERO, Signal::Analog(-5.0)));
    assert_eq!(volts(efc[1].1), vout(WORD & 0xff00));
    assert_eq!(volts(efc[2].1), vout(WORD));
    assert!(latched < efc[1].0 && efc[1].0 < efc[2].0);
    assert_eq!(
        board.chip().cpu().registers.data[0] & 0xffff,
        u32::from(WORD),
        "QH gave back the first word"
    );
}

/// SPCR0: MSTR, BITS 12, SPBR 2, and CPOL 1: CPHA 0 for SPI mode 2,
/// CPHA 1 for mode 3 (QSMRM 4.3.1).
const MASTER_12_MODE_2: u16 = 0xb202;
const MASTER_12_MODE_3: u16 = 0xb302;
/// SPCR2: ENDQP 5, NEWQP 3.
const ENTRIES_3_TO_5: u16 = 0x0503;
/// The word smartclock's self-test writes to each of the hex DAC's
/// three entries (z3805a-3543b, 0x187aa).
const PATTERN: u16 = 0x555;

/// The program of smartclock's hex DAC self-test: `PATTERN` in three
/// 12-bit entries in mode 2, PCS held across them, then the same three
/// in mode 3, read back into D0 to D2.
fn self_test_program() -> Vec<u16> {
    let mut program = Vec::new();
    program.extend(store(PORTQS_WORD, PCS_IDLE));
    program.extend(store(PQSPAR, PCS_OUTPUTS));
    for entry in 0..6 {
        program.extend(store(TRANSMIT_RAM + 2 * entry, PATTERN));
    }
    let frame = [
        CONT | BITSE | HEX_DAC,
        CONT | BITSE | HEX_DAC,
        BITSE | HEX_DAC,
    ];
    for (pair, commands) in frame.repeat(2).chunks(2).enumerate() {
        let word = u16::from(commands[0]) << 8 | u16::from(commands[1]);
        program.extend(store(COMMAND_RAM + 2 * pair as u32, word));
    }
    program.extend(store(SPCR0, MASTER_12_MODE_2));
    program.extend(store(SPCR2, ENTRIES_0_TO_2));
    program.extend(store(SPCR1, SPE));
    program.extend(await_spif());
    program.extend(store(SPCR0, MASTER_12_MODE_3));
    program.extend(store(SPCR2, ENTRIES_3_TO_5));
    program.extend(store(SPCR1, SPE));
    program.extend(await_spif());
    for register in 0..3 {
        let read_back = RECEIVE_RAM + 2 * (3 + register);
        program.extend([
            MOVE_WORD_TO_D0 | (register as u16) << 9,
            (read_back >> 16) as u16,
            read_back as u16,
        ]);
    }
    program.push(BRA_SELF);
    program
}

#[test]
fn the_hex_dac_reads_back_a_bit_early_in_mode_3() {
    let directory =
        env::temp_dir().join(format!("mc683xx-peripherals-hex-modes-{}", process::id()));
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("rom.bin"), image(&self_test_program())).unwrap();
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "hex"
type = "mc144110"
attach = [{ qspi = 0b1110 }]
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    mc683xx_peripherals::register(&mut registry).unwrap();
    let mut board =
        Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    board.run_until(Time(10_000_000_000)).unwrap();
    // Dout read on the rise after each shifting fall (MC144110/D 4.1;
    // QSMRM 4.3.1): each word a bit up, the next word's first bit in
    // its place; the firmware shifts it back down
    let data = board.chip().cpu().registers.data;
    let read: Vec<u32> = data[..3].iter().map(|register| register & 0xfff).collect();
    assert_eq!(read, [0xaaa, 0xaaa, 0xaaa]);
    assert!(read.iter().all(|word| word >> 1 == u32::from(PATTERN)));
}
