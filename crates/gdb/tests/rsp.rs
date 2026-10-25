//! The stub driven over an in-memory connection with raw remote serial
//! protocol packets, as gdb sends them (the gdb manual, Remote
//! Protocol): no gdb needed.  Each packet goes once the stub has
//! answered the last, as gdb waits; a Ctrl-C goes while it runs.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::env;
use std::error::Error;
use std::fmt::Display;
use std::fs;
use std::io;
use std::iter;
use std::marker;
use std::net::TcpStream;
use std::ops::ControlFlow;
use std::path::Path;
use std::process;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;

use gdbstub::conn::Connection;
use gdbstub::conn::ConnectionExt;
use mc683xx_board::board::Board;
use mc683xx_board::description::Description;
use mc683xx_board::description::Options;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::Registry;
use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::cpu::bare::End;
use mc683xx_cpu32::cpu::bare::Limits;
use mc683xx_cpu32::flat::ADDRESS_BITS;
use mc683xx_cpu32::flat::Machine;
use mc683xx_gdb::stub::Ending;
use mc683xx_gdb::stub::POLL;
use mc683xx_gdb::stub::PollError;
use mc683xx_gdb::stub::SessionError;
use mc683xx_gdb::stub::Waiter;
use mc683xx_gdb::stub::accept_polled;
use mc683xx_gdb::stub::serve;
use mc683xx_gdb::target::Target;
use mc683xx_mc68331::chip::Builder;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_modules::chip_select::ChipSelect;

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

/// What the test sends: a packet once the last is answered, or bytes
/// at once.
enum Send {
    /// A packet's body.
    Packet(String),
    /// Raw bytes, sent as soon as the stub reads.
    Now(Vec<u8>),
}

/// A packet's body, sent once the last is answered.
fn packet(body: &str) -> Send {
    Send::Packet(body.to_owned())
}

/// The connection: the script in, what the stub wrote out.
struct Wire {
    /// What is left to send.
    script: VecDeque<Send>,
    /// The bytes sent, not yet read.
    input: VecDeque<u8>,
    /// What the stub wrote.
    output: Rc<RefCell<Vec<u8>>>,
    /// Packets the stub had begun when the last packet went.
    answered: Option<usize>,
}

impl Wire {
    /// Sends the next of the script if the stub may read it.
    fn refill(&mut self) {
        if !self.input.is_empty() {
            return;
        }
        let begun = self.output.borrow().iter().filter(|&&b| b == b'$').count();
        let ready = match self.script.front() {
            Some(Send::Now(_)) => true,
            Some(Send::Packet(_)) => self.answered.is_none_or(|before| begun > before),
            None => false,
        };
        if !ready {
            return;
        }
        match self.script.pop_front() {
            Some(Send::Now(bytes)) => self.input.extend(bytes),
            Some(Send::Packet(body)) => {
                let sum = body.bytes().fold(0u8, u8::wrapping_add);
                self.input.extend(format!("${body}#{sum:02x}").bytes());
                self.answered = Some(begun);
            }
            None => {}
        }
    }
}

impl Connection for Wire {
    type Error = io::Error;

    fn write(&mut self, byte: u8) -> io::Result<()> {
        self.output.borrow_mut().push(byte);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl ConnectionExt for Wire {
    fn read(&mut self) -> io::Result<u8> {
        self.refill();
        self.input
            .pop_front()
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))
    }

    fn peek(&mut self) -> io::Result<Option<u8>> {
        self.refill();
        Ok(self.input.front().copied())
    }
}

/// The packets the stub sent, their bodies run-length decoded.
fn replies(output: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(output);
    text.split('$')
        .skip(1)
        .filter_map(|packet| packet.split_once('#'))
        .map(|(body, _)| {
            let mut decoded = String::new();
            let mut chars = body.chars();
            while let Some(c) = chars.next() {
                match c {
                    '*' => {
                        let count = chars.next().map_or(0, |n| n as usize - 29);
                        let last = decoded.chars().last().unwrap_or('0');
                        decoded.extend(iter::repeat_n(last, count));
                    }
                    other => decoded.push(other),
                }
            }
            decoded
        })
        .collect()
}

/// How a session ended.
type Served<T> = Result<Ending, SessionError<<T as Target>::Error, io::Error>>;

/// Serves `script` to `target`: how it ended and the stub's replies.
fn session<T: Target>(target: &mut T, script: Vec<Send>) -> (Served<T>, Vec<String>)
where
    T::Error: Display,
{
    let output = Rc::new(RefCell::new(Vec::new()));
    let wire = Wire {
        script: script.into(),
        input: VecDeque::new(),
        output: Rc::clone(&output),
        answered: None,
    };
    let ending = serve(target, wire);
    let replies = replies(&output.borrow());
    (ending, replies)
}

/// A `g` reply's registers.
fn registers(reply: &str) -> Vec<u32> {
    (0..reply.len() / 8)
        .map(|index| u32::from_str_radix(&reply[8 * index..8 * index + 8], 16).unwrap())
        .collect()
}

/// `words` as big-endian bytes.
fn bytes(words: &[u16]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_be_bytes()).collect()
}

/// `text` as hex, as `qRcmd` carries it.
fn hex(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02x}")).collect()
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
fn a_board_runs_under_the_stub() {
    let directory = env::temp_dir().join(format!("mc683xx-gdb-{}", process::id()));
    fs::create_dir_all(&directory).unwrap();
    let mut rom = vec![0; 0x1_0000];
    rom[..4].copy_from_slice(&STACK.to_be_bytes());
    rom[4..8].copy_from_slice(&START.to_be_bytes());
    let program = bytes(&PROGRAM);
    rom[START as usize..START as usize + program.len()].copy_from_slice(&program);
    fs::write(directory.join("rom.bin"), rom).unwrap();
    let text = r#"
        version = 1

        [chip]
        type = "mc68331"
        reference_hz = 32768
        [[memory]]
        name = "rom"
        image = "rom.bin"
        size = 0x10000
        attach = [{ select = "CSBOOT", role = "read" }]
    "#;
    let description = Description::parse(text, Path::new(&directory), &Options::default()).unwrap();
    let mut board = Board::<Mc68331>::load(
        &description,
        &Registry::with_builtins(),
        Environment::default(),
    )
    .unwrap();
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
    fs::remove_dir_all(&directory).unwrap();
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
