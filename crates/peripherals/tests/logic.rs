//! The octal latch and buffer on a board: the CPU writes the latch and
//! reads the buffer through one select, the latch's Q on nets and the
//! buffer's inputs from the board's file and a run-time endpoint.

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
/// Where the program starts.
const START: u32 = 0x400;
/// The boot ROM's size.
const ROM_BYTES: usize = 0x1_0000;
/// CSBAR1 and CSOR1 with MM set (MC68331UM Table D-3).
const CSBAR1: u32 = 0xff_fa50;
/// CSOR1.
const CSOR1: u32 = 0xff_fa52;
/// CS1 at $200000, a 64-Kbyte block (SIMRM 7.10.2, Table 7-6).
const CS1_BAR: u16 = 0x2003;
/// Asynchronous, both bytes, read and write, fast termination,
/// supervisor or user space (SIMRM 7.10.3).
const CS1_OPTIONS: u16 = 0x7bb0;
/// CS1's block.
const CS1_BASE: u32 = 0x20_0000;
/// MOVE.W #imm,(xxx).L (CPU32RM 4, MOVE).
const MOVE_WORD_TO_ABSOLUTE: u16 = 0x33fc;
/// MOVE.B #imm,(xxx).L.
const MOVE_BYTE_TO_ABSOLUTE: u16 = 0x13fc;
/// MOVE.B (xxx).L,D0.
const MOVE_BYTE_TO_D0: u16 = 0x1039;
/// BRA.S to itself.
const BRA_SELF: u16 = 0x60fe;
/// NOP.
const NOP: u16 = 0x4e71;
/// Where the latch and buffer answer on no select, with their own
/// decode: above CSBOOT's reset block (MC68331UM Table 4-24).
const BUS_BASE: u32 = 0x80_0000;
/// What the program writes to the latch.
const WRITTEN: u8 = 0xa5;

/// Scratch directories made, so that tests running at once never share one.
static DIRECTORIES: AtomicUsize = AtomicUsize::new(0);

/// `opcode` #`value`,`address`.
fn store(opcode: u16, address: u32, value: u16) -> [u16; 4] {
    [opcode, value, (address >> 16) as u16, address as u16]
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

/// The board: the latch and the switch buffer sharing CS1 by role, Q0
/// to Q7 to externals `q0` to `q7`, the buffer's A0 from `switch0` and
/// all its inputs from `switches`.
fn board() -> Board<Mc68331> {
    board_on(false)
}

/// The board, the latch and buffer on CS1, or `on_bus`, on no select at
/// `BUS_BASE` by their own decode, driving DSACK1.
fn board_on(on_bus: bool) -> Board<Mc68331> {
    let base = if on_bus { BUS_BASE } else { CS1_BASE };
    let program: Vec<u16> = [
        store(MOVE_WORD_TO_ABSOLUTE, CSBAR1, CS1_BAR),
        store(MOVE_WORD_TO_ABSOLUTE, CSOR1, CS1_OPTIONS),
        store(MOVE_BYTE_TO_ABSOLUTE, base, u16::from(WRITTEN)),
    ]
    .concat()
    .into_iter()
    .chain([MOVE_BYTE_TO_D0, (base >> 16) as u16, base as u16, BRA_SELF])
    .collect();
    let unique = DIRECTORIES.fetch_add(1, Ordering::Relaxed);
    let directory = env::temp_dir().join(format!(
        "mc683xx-peripherals-logic-{}-{unique}",
        process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("rom.bin"), image(&program)).unwrap();
    let mut text = String::from(
        r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "u1"
type = "74x374"
attach = [{ select = "CS1", role = "write" }]
reset = []
[[peripheral]]
name = "u2"
type = "74x244"
attach = [{ select = "CS1", role = "read" }]
parameters = { levels = 0xf0 }
[[net]]
name = "switch0"
type = "logic"
from = "external.switch0"
to = ["u2.A0"]
[[net]]
name = "switches"
type = "bus"
from = "external.switches"
to = ["u2.A"]
"#,
    );
    if on_bus {
        let window = format!("base = {BUS_BASE:#x}, size = 0x100");
        text = text
            .replace(
                "attach = [{ select = \"CS1\", role = \"write\" }]",
                &format!("attach = [{{ {window}, role = \"write\" }}]\ndsack = \"DSACK1\""),
            )
            .replace(
                "attach = [{ select = \"CS1\", role = \"read\" }]",
                &format!("attach = [{{ {window}, role = \"read\" }}]\ndsack = \"DSACK1\""),
            );
    }
    for bit in 0..8 {
        text.push_str(&format!(
            "[[net]]\nname = \"q{bit}\"\ntype = \"logic\"\nfrom = \"u1.Q{bit}\"\nto = [\"external.q{bit}\"]\n"
        ));
    }
    let description = Description::parse(&text, &directory, &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    mc683xx_peripherals::register(&mut registry).unwrap();
    let board = Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    board
}

#[test]
fn the_cpu_writes_the_latch_onto_nets_and_reads_the_buffer() {
    let mut board = board();
    board.send("switch0", Signal::Level(true)).unwrap();
    board.run_until(Time(100_000_000)).unwrap();
    for bit in 0..8 {
        let received = board.take_received(&format!("q{bit}")).unwrap();
        let high = WRITTEN & 1 << bit != 0;
        let expected: &[bool] = if high { &[false, true] } else { &[false] };
        assert_eq!(
            received
                .iter()
                .map(|(_, signal)| *signal)
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|level| Signal::Level(*level))
                .collect::<Vec<_>>(),
            "Q{bit}: low from power-on, then the byte written"
        );
        assert_eq!(received[0].0, Time::ZERO);
        if high {
            assert!(received[1].0 > Time::ZERO);
        }
    }
    assert_eq!(
        board.chip().cpu().registers.data[0] & 0xff,
        0xf1,
        "the file's levels, A0 from the endpoint"
    );
    let snapshot = board.save().unwrap();
    let mut restored = self::board();
    restored.restore(&snapshot).unwrap();
    assert_eq!(restored.save().unwrap(), snapshot);
}

#[test]
fn a_latch_and_buffer_on_no_select_decode_the_bus_themselves() {
    let mut board = board_on(true);
    board.send("switch0", Signal::Level(true)).unwrap();
    board.run_until(Time(100_000_000)).unwrap();
    let q0 = board.take_received("q0").unwrap();
    assert_eq!(
        q0.iter().map(|(_, signal)| *signal).collect::<Vec<_>>(),
        [Signal::Level(false), Signal::Level(true)],
        "the byte written reaches Q"
    );
    assert_eq!(board.chip().cpu().registers.data[0] & 0xff, 0xf1);
}

#[test]
fn a_host_sets_the_whole_switch_bank_through_one_external() {
    let mut board = board();
    board.send("switches", Signal::Bus(0x3c)).unwrap();
    board.run_until(Time(100_000_000)).unwrap();
    assert_eq!(board.chip().cpu().registers.data[0] & 0xff, 0x3c);
}
