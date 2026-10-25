//! The board layer (`mc683xx-board`): a board file's peripherals on a chip
//! select or a port pin, the firmware writing one in a loop, a word a
//! write.  A peripheral that drives nothing costs a call; one whose outputs
//! change at each write (a 74x374 whose Q0 feeds a chip pin) also costs
//! the board its nets' settling and a quantum, as does a port pin that
//! changes at each write on a net a peripheral senses (docs/performance.md).

use std::env;
use std::fs;
use std::hint::black_box;

use criterion::Criterion;
use criterion::Throughput;
use criterion::criterion_group;
use criterion::criterion_main;
use mc683xx_bench::kernel::MOVE_WORD_TO_ABSOLUTE;
use mc683xx_bench::kernel::NOP;
use mc683xx_board::board::Board;
use mc683xx_board::description::Description;
use mc683xx_board::description::Options;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::Registry;
use mc683xx_mc68331::chip::Mc68331;

/// The supervisor stack and the program's start, as reset reads them.
const STACK: u32 = 0x10_fffe;
/// Where the program starts, past the vectors.
const START: u32 = 0x400;
/// The boot ROM's bytes.
const ROM_BYTES: usize = 0x1_0000;
/// The SIM's registers with MM set (MC68331UM Table D-3).
const SIM: u32 = 0xff_fa00;
/// SYPCR's word: zero stops the watchdog (MC68331UM 4.2.10).
const SYPCR: u32 = SIM + 0x20;
/// CSBAR1 (MC68331UM D.2.19).
const CSBAR1: u32 = SIM + 0x50;
/// CSOR1 (MC68331UM D.2.20).
const CSOR1: u32 = SIM + 0x52;
/// CS1 at $200000, a 64-Kbyte block (SIMRM 7.10.2, Table 7-6).
const CS1_BAR: u16 = 0x2003;
/// Both bytes, read and write, fast termination, supervisor and user
/// space (SIMRM 7.10.3).
const CS_FAST: u16 = 0x7bb0;
/// CS1's window.
const CS1_BASE: u32 = 0x20_0000;
/// PFPAR's word: zero makes port F I/O (MC68331UM D.3.11).
const PFPAR: u32 = SIM + 0x1e;
/// DDRF's word: PF0 an output (MC68331UM D.3.10).
const DDRF: u32 = SIM + 0x1c;
/// PORTF0's word (MC68331UM D.3.9).
const PORTF0: u32 = SIM + 0x18;
/// MOVE.W D0,(xxx).L (CPU32RM 4, MOVE).
const MOVE_D0_TO_ABSOLUTE: u16 = 0x33c0;
/// ADDQ.W #1,D0.
const ADDQ_ONE_D0: u16 = 0x5240;
/// BRA.S back over the four words before it.
const BRA_BACK_FOUR: u16 = 0x60f6;
/// Board time one iteration runs: 1 ms.
const SPAN: u64 = 1_000_000_000;

/// Each case: its name, where the firmware writes, and its peripherals.  On
/// CS1 a register file, which drives nothing, and a 74x374 whose Q0 is
/// on PQS0, which changes at each write; on PF0, which toggles at each
/// write, nothing, then an edge counter.
const PERIPHERALS: [(&str, u32, &str); 4] = [
    (
        "registers_writes",
        CS1_BASE,
        r#"
[[peripheral]]
name = "peripheral"
type = "registers"
attach = [{ select = "CS1" }]
parameters = { bytes = 2 }
"#,
    ),
    (
        "latch_writes",
        CS1_BASE,
        r#"
[[peripheral]]
name = "peripheral"
type = "74x374"
attach = [{ select = "CS1", role = "write", lane = "lower" }]
reset = []
[[net]]
name = "q0"
type = "logic"
from = "peripheral.Q0"
to = ["chip.PQS0"]
"#,
    ),
    ("port_unwired", PORTF0, ""),
    (
        "port_sensed",
        PORTF0,
        r#"
[[peripheral]]
name = "counter"
type = "edge-counter"
[[net]]
name = "pf0"
type = "edge"
from = "chip.PF0"
to = ["counter.in"]
"#,
    ),
];

/// MOVE.W #`value`,`address`.
fn store(address: u32, value: u16) -> [u16; 4] {
    [
        MOVE_WORD_TO_ABSOLUTE,
        value,
        (address >> 16) as u16,
        address as u16,
    ]
}

/// The boot ROM: the watchdog stopped, CS1 mapped, PF0 an output, then
/// D0 written to `target` and incremented for ever.
fn image(target: u32) -> Vec<u8> {
    let program: Vec<u16> = [
        store(SYPCR, 0),
        store(CSBAR1, CS1_BAR),
        store(CSOR1, CS_FAST),
        store(PFPAR, 0),
        store(DDRF, 1),
    ]
    .concat()
    .into_iter()
    .chain([
        MOVE_D0_TO_ABSOLUTE,
        (target >> 16) as u16,
        target as u16,
        ADDQ_ONE_D0,
        BRA_BACK_FOUR,
    ])
    .collect();
    let mut image: Vec<u8> = NOP.to_be_bytes().repeat(ROM_BYTES / 2);
    image[0..4].copy_from_slice(&STACK.to_be_bytes());
    image[4..8].copy_from_slice(&START.to_be_bytes());
    for (index, word) in program.iter().enumerate() {
        let at = START as usize + 2 * index;
        image[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
    image
}

/// The board with `peripherals`, writing `target`, through its boot code.
fn board(name: &str, target: u32, peripherals: &str) -> Board<Mc68331> {
    let directory = env::temp_dir().join(format!("mc683xx-bench-{name}-{}", std::process::id()));
    fs::create_dir_all(&directory).expect("a scratch directory");
    fs::write(directory.join("rom.bin"), image(target)).expect("the image");
    let text = format!(
        "version = 1\n[chip]\ntype = \"mc68331\"\n[[memory]]\nname = \"rom\"\nimage = \"rom.bin\"\nattach = [{{ select = \"CSBOOT\" }}]\n{peripherals}"
    );
    let description =
        Description::parse(&text, &directory, &Options::default()).expect("the board file");
    fs::remove_dir_all(&directory).expect("the scratch directory removed");
    let mut registry = Registry::with_builtins();
    mc683xx_peripherals::register(&mut registry).expect("the stock peripherals");
    let mut board =
        Board::<Mc68331>::load(&description, &registry, Environment::default()).expect("the board");
    board.run_for(SPAN).expect("the boot");
    board
}

fn peripherals(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("peripherals");
    for (name, target, peripherals) in PERIPHERALS {
        let mut board = board(name, target, peripherals);
        let before = board.chip().instructions();
        board.run_for(SPAN).expect("a run");
        let instructions = board.chip().instructions() - before;
        eprintln!("{name}: {instructions} instructions an iteration");
        group.throughput(Throughput::Elements(instructions));
        group.bench_function(name, |bencher| {
            bencher.iter(|| {
                board
                    .run_until(black_box(board.now() + SPAN))
                    .expect("a run")
            });
        });
    }
    group.finish();
}

criterion_group!(benches, peripherals);
criterion_main!(benches);
