//! The stub driven over an in-memory connection with raw remote serial
//! protocol packets, as gdb sends them (the gdb manual, Remote
//! Protocol): no gdb needed.  Each packet goes once the stub has
//! answered the last, as gdb waits; a Ctrl-C goes while it runs.

use std::cell::RefCell;
use std::env;
use std::error::Error;
use std::fmt::Display;
use std::fs;
use std::io;
use std::marker;
use std::net::TcpStream;
use std::ops::ControlFlow;
use std::path::Path;
use std::process;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;

use gdbstub::conn::ConnectionExt;
use mc683xx_board::board::Board;
use mc683xx_board::description::Description;
use mc683xx_board::description::Options;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::Registry;
use mc683xx_board::signal::Signal;
use mc683xx_cpu32::cpu::bare::End;
use mc683xx_cpu32::cpu::bare::Limits;
use mc683xx_cpu32::flat::ADDRESS_BITS;
use mc683xx_cpu32::flat::Machine;
use mc683xx_gdb::Ending;
use mc683xx_gdb::SessionError;
use mc683xx_gdb::connection::POLL;
use mc683xx_gdb::connection::PollError;
use mc683xx_gdb::connection::Waiter;
use mc683xx_gdb::connection::accept_polled;
use mc683xx_gdb::serve;
use mc683xx_gdb::target::Target;
use mc683xx_mc68331::chip::Builder;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_mc68331::event::Event;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::sim::ResetCause;
use mc683xx_target::address::Address;
use wire::Send;
use wire::Wire;
use wire::bytes;
use wire::hex;
use wire::packet;
use wire::registers;
use wire::replies;

mod wire;

/// The initial SSP.
const STACK: u32 = 0x8000;
/// Where the program starts.
const START: u32 = 0x1000;
/// Where it stores D0.
const STORE: u32 = 0x2000;
/// The memory the bare CPU maps from zero.
const MAPPED: u32 = 0x1_0000;
/// The program (CPU32RM 4): `moveq #5,d0`, `addq.l #1,d0`, `move.l
/// d0,(a0)`, `bra.s` back to its start.
const PROGRAM: [u16; 4] = [0x7005, 0x5280, 0x2080, 0x60f8];
/// The store's address in the program.
const STORE_PC: u32 = START + 4;
/// The branch's address.
const BRANCH_PC: u32 = START + 6;
/// SR: supervisor, interrupts masked.
const SUPERVISOR: u16 = 0x2700;
/// An address nothing maps.
const UNMAPPED: u32 = 0x9000_0000;
/// gdb's Ctrl-C.
const INTERRUPT: u8 = 0x03;
/// The `g` packet's registers: the core feature's 18, then 5 system.
const REGISTERS: usize = 23;
/// PC's index in a `g` packet.
const PC: usize = 17;
/// D1's.
const D1: usize = 1;
/// SSP's.
const SSP: usize = 19;
/// The SIM's SIMCR with MM set (MC68331UM Table D-1), which reads $00CF
/// out of reset (MC68331UM D.2.1).
const SIMCR: u32 = 0xff_fa00;
/// SIMCR's reset value.
const SIMCR_RESET: u32 = 0x00cf;

/// How a session ended.
type Served<T> = Result<Ending, SessionError<<T as Target>::Error, io::Error>>;

/// Serves `script` to `target`: how it ended and the stub's replies.
fn session<T: Target>(target: &mut T, script: Vec<Send>) -> (Served<T>, Vec<String>)
where
    T::Error: Display,
{
    let output = Rc::new(RefCell::new(Vec::new()));
    let wire = Wire::new(script, &output);
    let ending = serve(target, wire);
    let replies = replies(&output.borrow());
    (ending, replies)
}

/// A bare CPU with the program at `START`, the reset vectors at zero,
/// reset.
fn machine() -> Machine {
    let mut machine = Machine::new(ADDRESS_BITS).unwrap();
    machine.map(Address(0), MAPPED, &[]).unwrap();
    let vectors = [STACK.to_be_bytes(), START.to_be_bytes()].concat();
    machine.write(Address(0), &vectors).unwrap();
    machine.write(Address(START), &bytes(&PROGRAM)).unwrap();
    machine.reset().unwrap();
    machine.cpu.registers.set_address(0, STORE);
    machine
}

#[test]
fn registers_and_the_description() {
    let mut machine = machine();
    let (ending, replies) = session(
        &mut machine,
        vec![
            packet("?"),
            packet("g"),
            packet("qXfer:features:read:target.xml:0,fff"),
            packet("D"),
        ],
    );
    assert_eq!(ending.unwrap(), Ending::Detached);
    assert!(replies[0].starts_with("T05"));
    let read = registers(&replies[1]);
    assert_eq!(read.len(), REGISTERS);
    assert_eq!(read[PC], START);
    assert_eq!(read[SSP], STACK);
    assert_eq!(read[8], STORE);
    assert!(replies[2].contains("m68k:cpu32"));
    assert!(replies[2].contains("org.gnu.gdb.m68k.core"));
    assert!(!replies[2].contains("fp0"));
}

#[test]
fn a_description_there_is_none_of_is_an_error_reply() {
    let mut machine = machine();
    let (ending, replies) = session(
        &mut machine,
        vec![
            packet("qXfer:features:read:nope.xml:0,10"),
            packet("qXfer:features:read:target.xml:0,5"),
            packet("qXfer:features:read:target.xml:fffff,10"),
            packet("D"),
        ],
    );
    assert_eq!(ending.unwrap(), Ending::Detached);
    assert_eq!(replies[0], "E00");
    assert_eq!(replies[1], "m<?xml");
    assert_eq!(replies[2], "l");
}

#[test]
fn register_writes_reach_the_cpu() {
    let mut machine = machine();
    let mut words = [0u32; REGISTERS];
    words[D1] = 0x1234_5678;
    words[16] = SUPERVISOR.into();
    words[PC] = BRANCH_PC;
    words[SSP] = STACK;
    words[15] = STACK;
    let body: String = words.iter().map(|word| format!("{word:08x}")).collect();
    let (ending, replies) = session(&mut machine, vec![packet(&format!("G{body}")), packet("D")]);
    ending.unwrap();
    assert_eq!(replies[0], "OK");
    let registers = &machine.cpu.registers;
    assert_eq!(registers.data[1], 0x1234_5678);
    assert_eq!(registers.pc, BRANCH_PC);
    assert_eq!(registers.ssp, STACK);
}

#[test]
fn a_short_g_sets_what_it_gives_and_a_ragged_one_is_refused() {
    let mut machine = machine();
    let (ending, replies) = session(
        &mut machine,
        vec![
            packet("G00"),
            packet("G0000000112345678"),
            packet("g"),
            packet("D"),
        ],
    );
    assert_eq!(ending.unwrap(), Ending::Detached);
    assert_eq!(replies[0], "E16");
    assert_eq!(replies[1], "OK");
    let read = registers(&replies[2]);
    assert_eq!(read[..2], [1, 0x1234_5678]);
    assert_eq!(read[PC], START);
    assert_eq!(read[SSP], STACK);
}

#[test]
fn memory_without_cycles() {
    let mut machine = machine();
    let (ending, replies) = session(
        &mut machine,
        vec![
            packet(&format!("m{START:x},4")),
            packet(&format!("M{STORE:x},2:abcd")),
            packet(&format!("m{STORE:x},2")),
            packet(&format!("m{UNMAPPED:x},4")),
            packet(&format!("M{UNMAPPED:x},1:00")),
            packet("D"),
        ],
    );
    ending.unwrap();
    assert_eq!(replies[0], "70055280");
    assert_eq!(replies[1], "OK");
    assert_eq!(replies[2], "abcd");
    assert_eq!(replies[3], "E0e");
    assert_eq!(replies[4], "E0e");
}

#[test]
fn steps_one_instruction_at_a_time() {
    let mut machine = machine();
    let (ending, replies) = session(
        &mut machine,
        vec![
            packet("s"),
            packet("s"),
            packet("s"),
            packet("g"),
            packet("D"),
        ],
    );
    ending.unwrap();
    assert_eq!(replies[..3], ["S05", "S05", "S05"]);
    assert_eq!(registers(&replies[3])[PC], BRANCH_PC);
    assert_eq!(machine.cpu.registers.data[0], 6);
    assert_eq!(machine.cpu.instructions(), 3);
}

#[test]
fn breakpoints_stop_before_and_continue_past() {
    let mut machine = machine();
    let (ending, replies) = session(
        &mut machine,
        vec![
            packet(&format!("Z0,{STORE_PC:x},2")),
            packet("c"),
            packet("c"),
            packet(&format!("z0,{STORE_PC:x},2")),
            packet(&format!("Z1,{BRANCH_PC:x},2")),
            packet("s"),
            packet("c"),
            packet("D"),
        ],
    );
    ending.unwrap();
    assert_eq!(replies[0], "OK");
    assert!(replies[1].starts_with("T05") && replies[1].contains("swbreak"));
    assert!(replies[2].contains("swbreak"));
    assert_eq!(replies[3], "OK");
    assert_eq!(replies[4], "OK");
    // The step from the software breakpoint stops at the hardware one,
    // and the continue runs past it and round the loop to it again.
    assert_eq!(replies[5], "S05");
    assert!(replies[6].contains("hwbreak"));
    assert_eq!(machine.cpu.registers.pc, BRANCH_PC);
    // Two loops and a store: the second continue went round once more.
    assert_eq!(machine.cpu.instructions(), 2 + 4 + 1 + 4);
}

#[test]
fn watchpoints_stop_after_the_access() {
    let mut machine = machine();
    let (ending, replies) = session(
        &mut machine,
        vec![
            packet(&format!("Z2,{:x},1", STORE + 3)),
            packet("c"),
            packet(&format!("z2,{:x},1", STORE + 3)),
            packet(&format!("Z4,{STORE:x},2")),
            packet("c"),
            packet("D"),
        ],
    );
    ending.unwrap();
    assert_eq!(replies[0], "OK");
    // The long store's last byte: the stop comes once it has stored.
    assert_eq!(replies[1], format!("T05thread:01;watch:{:x};", STORE + 3));
    assert_eq!(replies[2], "OK");
    assert_eq!(replies[4], format!("T05thread:01;awatch:{STORE:x};"));
    assert_eq!(machine.cpu.registers.pc, BRANCH_PC);
    assert_eq!(machine.cpu.instructions(), 3 + 4);
}

#[test]
fn read_watchpoints_see_reads() {
    let mut machine = machine();
    // move.l (a0),d1, then branch to itself.
    let reader = [0x2210, 0x60fe];
    machine.write(Address(START), &bytes(&reader)).unwrap();
    let (ending, replies) = session(
        &mut machine,
        vec![packet(&format!("Z3,{STORE:x},4")), packet("c"), packet("D")],
    );
    ending.unwrap();
    assert_eq!(replies[0], "OK");
    assert_eq!(replies[1], format!("T05thread:01;rwatch:{STORE:x};"));
    assert_eq!(machine.cpu.registers.pc, START + 2);
}

#[test]
fn ctrl_c_interrupts_a_run() {
    let mut machine = machine();
    let (ending, replies) = session(
        &mut machine,
        vec![packet("c"), Send::Now(vec![INTERRUPT]), packet("D")],
    );
    ending.unwrap();
    assert_eq!(replies[0], "S02");
    assert!(machine.cpu.instructions() > 4);
}

#[test]
fn ctrl_c_while_stopped_is_nothing() {
    // Ctrl-C with the target stopped, then a continue: it runs to the
    // breakpoint, not stopping at once for the Ctrl-C
    let mut machine = machine();
    let (ending, replies) = session(
        &mut machine,
        vec![
            packet(&format!("Z0,{STORE_PC:x},2")),
            Send::Now(vec![INTERRUPT]),
            packet("c"),
            packet("D"),
        ],
    );
    ending.unwrap();
    assert_eq!(replies[0], "OK");
    assert!(replies[1].starts_with("T05") && replies[1].contains("swbreak"));
    assert_eq!(machine.cpu.registers.pc, STORE_PC);
}

#[test]
fn monitor_reset_and_kill() {
    let mut machine = machine();
    let (ending, replies) = session(
        &mut machine,
        vec![
            packet("s"),
            packet(&format!("qRcmd,{}", hex("reset"))),
            packet(&format!("qRcmd,{}", hex("bogus"))),
            packet("k"),
        ],
    );
    assert_eq!(ending.unwrap(), Ending::Killed);
    assert!(replies.iter().any(|reply| reply == "OK"));
    assert_eq!(machine.cpu.registers.pc, START);
}

#[test]
fn its_hooks_go_with_the_session() {
    let mut machine = machine();
    let (ending, _) = session(
        &mut machine,
        vec![
            packet(&format!("Z0,{STORE_PC:x},2")),
            packet(&format!("Z2,{STORE:x},4")),
        ],
    );
    assert!(matches!(ending, Err(SessionError::Connection(_))));
    let mut limits = Limits::default();
    limits.instructions = Some(20);
    let run = machine.run_limited(limits);
    assert_eq!(run.end, End::Instructions);
}

/// The chip with the program in a boot ROM on CSBOOT, which decodes
/// the whole space out of reset.
fn chip() -> Mc68331 {
    let mut rom = vec![0; 2048 * 4];
    rom[..4].copy_from_slice(&STACK.to_be_bytes());
    rom[4..8].copy_from_slice(&START.to_be_bytes());
    let program = bytes(&PROGRAM);
    rom[START as usize..START as usize + program.len()].copy_from_slice(&program);
    let mut builder = Builder::default();
    builder.memory(rom, false, &[ChipSelect::Boot]).unwrap();
    builder.build()
}

#[test]
fn the_chip_peeks_and_pokes_without_cycles() {
    let mut chip = chip();
    let (ending, replies) = session(
        &mut chip,
        vec![
            packet(&format!("m{START:x},4")),
            packet(&format!("m{SIMCR:x},2")),
            packet(&format!("M{STORE:x},2:abcd")),
            packet(&format!("m{STORE:x},2")),
            packet(&format!("Z0,{BRANCH_PC:x},2")),
            packet("c"),
            packet("s"),
            packet(&format!("qRcmd,{}", hex("reset"))),
            packet("D"),
        ],
    );
    ending.unwrap();
    assert_eq!(replies[0], "70055280");
    assert_eq!(u32::from_str_radix(&replies[1], 16).unwrap(), SIMCR_RESET);
    // ROM takes the debugger's write.
    assert_eq!(replies[2], "OK");
    assert_eq!(replies[3], "abcd");
    assert!(replies[5].contains("swbreak"));
    assert_eq!(replies[6], "S05");
    assert_eq!(chip.cpu().registers.pc, START);
}

#[test]
fn monitor_fc_names_the_space_memory_goes_to() {
    // the bare CPU has one space; the chip writes in supervisor space
    // alone
    let mut machine = machine();
    let (ending, replies) = session(
        &mut machine,
        vec![
            packet(&format!("qRcmd,{}", hex("fc"))),
            packet(&format!("qRcmd,{}", hex("fc 6"))),
            packet(&format!("m{START:x},2")),
            packet("D"),
        ],
    );
    ending.unwrap();
    let said = |replies: &[String], text: &str| {
        replies
            .iter()
            .any(|reply| *reply == format!("O{}", hex(text)))
    };
    assert!(said(&replies, "fc 5\n"));
    assert!(said(&replies, "fc 6\n"));
    assert!(replies.iter().any(|reply| reply == "7005"));
    let mut chip = chip();
    let (ending, replies) = session(
        &mut chip,
        vec![
            packet(&format!("qRcmd,{}", hex("fc 7"))),
            packet(&format!("M{STORE:x},2:abcd")),
            packet(&format!("qRcmd,{}", hex("fc 6"))),
            packet(&format!("M{STORE:x},2:abcd")),
            packet("D"),
        ],
    );
    ending.unwrap();
    let written: Vec<&String> = replies
        .iter()
        .filter(|reply| *reply == "OK" || reply.starts_with('E'))
        .collect();
    // each monitor command, each write, then the detach
    assert_eq!(written, ["OK", "E0e", "OK", "OK", "OK"]);
}

/// A board with the program in a boot ROM on CSBOOT, and `more` in its
/// file; `name` names its directory.
fn board(name: &str, more: &str) -> Board<Mc68331> {
    let directory = env::temp_dir().join(format!("mc683xx-gdb-{name}-{}", process::id()));
    fs::create_dir_all(&directory).unwrap();
    let mut rom = vec![0; 0x1_0000];
    rom[..4].copy_from_slice(&STACK.to_be_bytes());
    rom[4..8].copy_from_slice(&START.to_be_bytes());
    let program = bytes(&PROGRAM);
    rom[START as usize..START as usize + program.len()].copy_from_slice(&program);
    fs::write(directory.join("rom.bin"), rom).unwrap();
    let text = format!(
        r#"
        version = 1

        [chip]
        type = "mc68331"
        reference_hz = 32768
        [[memory]]
        name = "rom"
        image = "rom.bin"
        size = 0x10000
        attach = [{{ select = "CSBOOT", role = "read" }}]
        {more}
    "#
    );
    let description =
        Description::parse(&text, Path::new(&directory), &Options::default()).unwrap();
    let board = Board::<Mc68331>::load(
        &description,
        &Registry::with_builtins(),
        Environment::default(),
    )
    .unwrap();
    fs::remove_dir_all(&directory).unwrap();
    board
}

#[test]
fn a_board_runs_under_the_stub() {
    let mut board = board("runs", "");
    let (ending, replies) = session(
        &mut board,
        vec![
            packet(&format!("Z0,{STORE_PC:x},2")),
            packet("c"),
            packet("s"),
            packet("c"),
            packet(&format!("z0,{STORE_PC:x},2")),
            packet("c"),
            Send::Now(vec![INTERRUPT]),
            packet("D"),
        ],
    );
    ending.unwrap();
    assert!(replies[1].contains("swbreak"));
    assert_eq!(replies[2], "S05");
    assert!(replies[3].contains("swbreak"));
    assert_eq!(replies[5], "S02");
    assert!(board.now().0 > 0);
}

#[test]
fn monitor_reset_on_a_board_drives_reset_as_the_board_does() {
    let mut board = board(
        "reset",
        r#"
        [[net]]
        name = "reset"
        type = "wired-and"
        from = ["chip.RESET"]
        to = ["external.observed"]
        "#,
    );
    board.take_events();
    board.take_received("observed").unwrap();
    let (ending, _) = session(&mut board, vec![monitor("reset"), packet("D")]);
    ending.unwrap();
    // reported at once, both edges' resets as a board-driven RESET's
    let causes: Vec<ResetCause> = board
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Reset(_, cause) => Some(cause),
            _ => None,
        })
        .collect();
    assert_eq!(causes, [ResetCause::External, ResetCause::External]);
    assert_eq!(
        board.chip().sim().reset_status(),
        ResetCause::External.status()
    );
    let now = board.now();
    board.run_until(now + 1_000_000_000).unwrap();
    let levels: Vec<Signal> = board
        .take_received("observed")
        .unwrap()
        .into_iter()
        .map(|(_, signal)| signal)
        .collect();
    assert_eq!(levels, [Signal::Level(false), Signal::Level(true)]);
}

/// What the stub said on gdb's console: the `O` packets' text.
fn console(replies: &[String]) -> String {
    replies
        .iter()
        .filter(|reply| reply.starts_with('O') && *reply != "OK")
        .map(|reply| {
            let hex = &reply[1..];
            let bytes: Vec<u8> = (0..hex.len() / 2)
                .map(|index| u8::from_str_radix(&hex[2 * index..2 * index + 2], 16).unwrap())
                .collect();
            String::from_utf8(bytes).unwrap()
        })
        .collect()
}

/// `monitor` `command`'s packet.
fn monitor(command: &str) -> Send {
    packet(&format!("qRcmd,{}", hex(command)))
}

#[test]
fn a_stop_condition_stops_a_board_with_a_trap_and_is_named() {
    let mut board = board(
        "conditions",
        r#"
        [[peripheral]]
        name = "reference"
        type = "clock"
        parameters = { hz = 10_000_000 }
        [[peripheral]]
        name = "divider"
        type = "divider"
        parameters = { divide = 1000 }
        [[peripheral]]
        name = "counter"
        type = "edge-counter"
        [[net]]
        name = "ten-megahertz"
        type = "clock"
        from = "reference.out"
        to = ["divider.in"]
        [[net]]
        name = "pps"
        type = "edge"
        from = "divider.out"
        to = ["counter.in"]
        "#,
    );
    let (ending, replies) = session(
        &mut board,
        vec![
            monitor("stop"),
            monitor("stop conditions"),
            monitor("stop net pps up"),
            monitor("stop net ten-megahertz rise"),
            monitor("stop net pps rise"),
            packet("c"),
            monitor("stop"),
            monitor("stop delete 1"),
            monitor("stop delete 1"),
            monitor("stop"),
            monitor("help"),
            packet("D"),
        ],
    );
    ending.unwrap();
    assert!(replies.contains(&"S05".to_owned()), "{replies:?}");
    assert_eq!(board.net_value("pps").unwrap(), Some(Signal::Level(true)));
    let console = console(&replies);
    for line in [
        "no stop conditions armed\n",
        "net NET rise|fall|change|high|low: ",
        "no net change \"up\"",
        "net \"ten-megahertz\" is a clock net: only `change` watches it\n",
        "1: net pps rise\n",
        "1: net pps rise (fired)\n",
        "deleted 1\n",
        "no condition 1\n",
        "stop conditions [PERIPHERAL]: ",
    ] {
        assert!(console.contains(line), "{line:?} in {console:?}");
    }
}

#[test]
fn a_target_with_no_conditions_says_so() {
    let mut machine = machine();
    let (ending, replies) = session(&mut machine, vec![monitor("stop"), packet("D")]);
    ending.unwrap();
    assert_eq!(console(&replies), "this target has no stop conditions\n");
}

#[test]
fn other_stops_are_signals() {
    let mut machine = machine();
    machine
        .cpu
        .add_code_hook(Address(STORE_PC)..=Address(STORE_PC), |_| {
            ControlFlow::Break(())
        });
    let (ending, replies) = session(&mut machine, vec![packet("c"), packet("D")]);
    ending.unwrap();
    // The host's own hook: a trap.
    assert_eq!(replies[0], "S05");
    let mut machine = self::machine();
    // An odd PC's address error, stacked where nothing is mapped: a
    // double bus fault (CPU32RM 6.2.1 to 6.2.3).
    machine.cpu.registers.pc = START + 1;
    machine.cpu.registers.ssp = UNMAPPED;
    let (ending, replies) = session(&mut machine, vec![packet("c"), packet("D")]);
    ending.unwrap();
    assert_eq!(replies[0], "S0a");
}

/// A host that counts its waits and idles, and ends the session at its
/// `last` idle.
struct Counting {
    /// Waits so far.
    waits: usize,
    /// Idles so far.
    idles: usize,
    /// The idle that ends the session.
    last: usize,
}

impl Waiter for Counting {
    type Error = usize;

    fn wait<R: marker::Send>(&mut self, wait: impl FnOnce() -> R + marker::Send) -> R {
        self.waits += 1;
        wait()
    }

    fn idle(&mut self) -> Result<(), usize> {
        self.idles += 1;
        if self.idles == self.last {
            Err(self.idles)
        } else {
            Ok(())
        }
    }
}

#[test]
fn a_polled_wait_ends_when_the_host_says() {
    let waiter = Counting {
        waits: 0,
        idles: 0,
        last: 3,
    };
    let accepted = accept_polled("127.0.0.1:0", |_| Ok(()), waiter);
    assert!(matches!(accepted, Err(PollError::Host(3))));
}

/// A host's error that has a cause.
#[derive(Debug, thiserror::Error)]
#[error("the host stopped")]
struct Stopped(#[source] io::Error);

#[test]
fn a_host_error_shows_as_its_own() {
    let error = PollError::Host(Stopped(io::Error::other("interrupted")));
    assert_eq!(error.to_string(), "the host stopped");
    assert_eq!(
        error.source().map(ToString::to_string).as_deref(),
        Some("interrupted")
    );
}

#[test]
fn a_polled_connection_reads_between_idles() {
    let (sender, port) = mpsc::channel();
    let client = thread::spawn(move || {
        let port: u16 = port.recv().unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        thread::sleep(POLL * 3);
        io::Write::write_all(&mut stream, b"+").unwrap();
        thread::sleep(POLL * 3);
    });
    let waiter = Counting {
        waits: 0,
        idles: 0,
        last: usize::MAX,
    };
    let mut connection = accept_polled(
        "127.0.0.1:0",
        |at| {
            sender.send(at.port()).unwrap();
            Ok(())
        },
        waiter,
    )
    .unwrap();
    assert_eq!(connection.read().unwrap(), b'+');
    assert!(matches!(connection.read(), Err(PollError::Connection(_))));
    client.join().unwrap();
}

/// An odd address, which the program's long store faults at (CPU32RM
/// 6.2.3).
const ODD: u32 = 0x2001;

/// `monitor faults` on `target`, its store at [`ODD`]: taken, then
/// stopping the run with `SIGBUS` and saying which fault.
fn monitor_faults_stop_the_run<T: Target>(target: &mut T)
where
    T::Error: Display + std::fmt::Debug,
{
    let (ending, replies) = session(
        target,
        vec![
            packet(&format!("qRcmd,{}", hex("faults"))),
            packet(&format!("qRcmd,{}", hex("faults stop"))),
            packet("c"),
            packet(&format!("qRcmd,{}", hex("faults"))),
            packet("k"),
        ],
    );
    assert_eq!(ending.unwrap(), Ending::Killed);
    let said: Vec<String> = replies
        .iter()
        .filter(|reply| *reply != "OK")
        .filter_map(|reply| reply.strip_prefix('O'))
        .map(|text| {
            let bytes = (0..text.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
                .collect();
            String::from_utf8(bytes).unwrap()
        })
        .collect();
    assert_eq!(said[0], "faults take\n");
    assert_eq!(said[1], "faults stop\n");
    // SIGBUS, gdb's 10
    assert!(replies.iter().any(|reply| reply == "S0a"), "{replies:?}");
    assert_eq!(
        said[2],
        format!(
            "faults stop\nstopped at address error: write of 4 bytes at {}, fc 5, pc {}\n",
            Address(ODD),
            Address(STORE_PC)
        )
    );
}

#[test]
fn monitor_faults_stop_the_bare_cpu_s_run() {
    let mut machine = machine();
    machine.cpu.registers.set_address(0, ODD);
    monitor_faults_stop_the_run(&mut machine);
    assert_eq!(machine.cpu.registers.pc, STORE_PC);
}

#[test]
fn monitor_faults_stop_the_chip_s_run() {
    let mut chip = chip();
    chip.cpu_control().registers_mut().set_address(0, ODD);
    monitor_faults_stop_the_run(&mut chip);
    assert_eq!(chip.cpu().registers.pc, STORE_PC);
}

#[test]
fn a_fault_stop_outranks_a_watch_hit_it_undid() {
    for resume in ["c", "s"] {
        let mut machine = machine();
        // TRAP #0, SSP 4: its frame's first words go to 2 and 0,
        // watched, then below zero, where nothing maps
        machine.write(Address(START), &bytes(&[0x4e40])).unwrap();
        machine.cpu.registers.ssp = 4;
        machine.cpu.set_stop_on_fault(true);
        let (ending, replies) = session(
            &mut machine,
            vec![packet("Z2,0,4"), packet(resume), packet("k")],
        );
        assert_eq!(ending.unwrap(), Ending::Killed);
        assert_eq!(replies[0], "OK");
        assert_eq!(replies[1], "S0a", "{resume}: SIGBUS: {replies:?}");
        assert_eq!(machine.cpu.registers.pc, START, "{resume}: the TRAP undone");
    }
}

#[test]
fn a_watch_hit_in_an_instruction_kept_through_its_trace_s_fault_is_reported() {
    for resume in ["c", "s"] {
        let mut machine = machine();
        // ADDQ.W #1,(A0) traced (T1), SSP where nothing maps: the ADDQ
        // completes, its trace's stacking faults (CPU32RM 6.2.10), and
        // the ADDQ is kept, its trace pending
        machine.write(Address(START), &bytes(&[0x5250])).unwrap();
        machine.cpu.registers.set_sr(0xa700);
        machine.cpu.registers.ssp = UNMAPPED;
        machine.cpu.set_stop_on_fault(true);
        let (ending, replies) = session(
            &mut machine,
            vec![
                packet(&format!("Z2,{STORE:x},2")),
                packet(resume),
                packet("k"),
            ],
        );
        assert_eq!(ending.unwrap(), Ending::Killed);
        assert!(
            replies[1].starts_with("T05") && replies[1].contains("watch"),
            "{resume}: {replies:?}"
        );
        assert_eq!(
            machine.cpu.registers.pc,
            START + 2,
            "{resume}: the ADDQ kept"
        );
        assert!(machine.cpu.trace_pending().is_some());
    }
}
