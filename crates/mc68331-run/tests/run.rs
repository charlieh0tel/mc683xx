//! The runner end to end: `examples/boards/echo.toml` with its
//! firmware, which echoes the SCI, its input piped in; and its exit status when
//! the CPU halts or the command is wrong; and a gdb session with it
//! (`--gdb`), over TCP with raw packets.

use std::env;
use std::fs;
use std::io;
use std::io::BufRead;
use std::io::BufReader;
use std::io::ErrorKind;
use std::io::Read;
use std::io::Write;
use std::net::TcpStream;
#[cfg(unix)]
use std::panic;
use std::path::Path;
use std::path::PathBuf;
use std::process;
use std::process::Child;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::sync::mpsc;
#[cfg(unix)]
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::Duration;

use mc683xx_examples::echo;
#[cfg(unix)]
use mc683xx_examples::firmware::RAM_BASE;
use mc683xx_examples::firmware::START;
use mc683xx_examples::firmware::rom;
#[cfg(unix)]
use mc683xx_gdb::Ending;
#[cfg(unix)]
use mc683xx_gdb::SessionError;
#[cfg(unix)]
use mc683xx_gdb::bdm::Bdm;
#[cfg(unix)]
use mc683xx_gdb::serve;
#[cfg(unix)]
use mc683xx_target::Cpu32Target;
#[cfg(unix)]
use mc683xx_target::address::Address;
#[cfg(unix)]
use mc683xx_target::entry::Entry;
#[cfg(unix)]
use mc683xx_target::function_code::FunctionCode;
#[cfg(unix)]
use mc683xx_target::register::Register;
#[cfg(unix)]
use mc683xx_target::register::SystemRegister;
#[cfg(unix)]
use mc683xx_target::size::Size;
#[cfg(unix)]
use nix::pty::openpty;
#[cfg(unix)]
use nix::sys::signal::Signal;
#[cfg(unix)]
use nix::sys::signal::kill;
#[cfg(unix)]
use nix::sys::termios::LocalFlags;
#[cfg(unix)]
use nix::sys::termios::OutputFlags;
#[cfg(unix)]
use nix::sys::termios::tcgetattr;
#[cfg(unix)]
use nix::unistd::Pid;
#[cfg(unix)]
use std::cell::RefCell;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::rc::Rc;
#[cfg(unix)]
use wire::Send;
#[cfg(unix)]
use wire::Wire;
#[cfg(unix)]
use wire::bytes;
#[cfg(unix)]
use wire::hex;
#[cfg(unix)]
use wire::registers;
#[cfg(unix)]
use wire::replies;

/// gdb's end of an in-memory connection, the gdb stub's tests' own.
#[cfg(unix)]
#[path = "../../gdb/tests/wire/mod.rs"]
mod wire;

/// TRAP #0 (CPU32RM 4, TRAP).
const TRAP: u16 = 0x4e40;

/// A scratch copy of the example board with `image` as `echo.bin`.
fn board(name: &str, image: &[u8]) -> PathBuf {
    let directory = env::temp_dir().join(format!("mc683xx-run-{name}-{}", process::id()));
    fs::create_dir_all(&directory).unwrap();
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/boards/echo.toml");
    fs::copy(example, directory.join("echo.toml")).unwrap();
    fs::write(directory.join("echo.bin"), image).unwrap();
    directory
}

/// The runner on `arguments`, `input` on its standard input.
fn runner(arguments: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mc683xx-run"))
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // a runner that refuses its command line exits without reading;
    // what it reads, the tests check through its output
    match child.stdin.take().unwrap().write_all(input) {
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}
        written => written.unwrap(),
    }
    child.wait_with_output().unwrap()
}

#[test]
fn the_example_board_echoes_its_console() {
    let directory = board("echo", &echo::image());
    let file = directory.join("echo.toml");
    let arguments = [
        file.to_str().unwrap(),
        "--bind",
        "console=stdio",
        "--seconds",
        "0.02",
        "--trace",
        "pins",
        "--input-after",
        "0.001",
    ];
    let output = runner(&arguments, b"hello");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    assert_eq!(output.stdout, b"hello");
    assert!(
        stderr.starts_with("# mc683xx trace 3 chip=mc68331\n"),
        "{stderr}"
    );
    assert!(
        stderr
            .lines()
            .last()
            .unwrap()
            .starts_with("mc68331: stopped cycle="),
        "{stderr}"
    );
    assert!(stderr.ends_with("reason=budget\n"), "{stderr}");
    let again = runner(&arguments, b"hello");
    assert_eq!(
        String::from_utf8(again.stderr).unwrap(),
        stderr,
        "deterministic"
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn cycles_end_the_run_within_an_instruction_of_them() {
    let directory = board("cycles", &echo::image());
    let file = directory.join("echo.toml");
    let output = runner(&[file.to_str().unwrap(), "--cycles", "100"], b"");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    let cycle: u64 = stderr
        .split_once("stopped cycle=")
        .and_then(|(_, rest)| rest.split(' ').next())
        .and_then(|cycle| cycle.parse().ok())
        .unwrap_or_else(|| panic!("{stderr}"));
    // The longest CPU32 instruction takes far fewer clocks than a turn's
    // 16,000 (CPU32RM 8).
    assert!((100..200).contains(&cycle), "{stderr}");
    fs::remove_dir_all(directory).unwrap();
}

/// `board(name, echo::image())`: the board file's path.
fn saving_board(name: &str) -> PathBuf {
    board(name, &echo::image()).join("echo.toml")
}

/// How many temporary snapshot files `directory` holds.
fn partials(directory: &Path) -> usize {
    fs::read_dir(directory)
        .unwrap()
        .filter(|entry| {
            let name = entry.as_ref().unwrap().file_name();
            name.to_string_lossy().ends_with(".partial")
        })
        .count()
}

/// The runner on `file` for a moment, saving to `saved`: its status
/// and standard error.
fn save(file: &Path, saved: &Path) -> (Option<i32>, String) {
    let output = runner(
        &[
            file.to_str().unwrap(),
            "--save",
            saved.to_str().unwrap(),
            "--seconds",
            "0.001",
        ],
        b"",
    );
    (
        output.status.code(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn a_save_that_cannot_be_made_is_refused_before_the_run() {
    let file = saving_board("save");
    let directory = file.parent().unwrap();
    let (status, stderr) = save(&file, &directory.join("missing").join("saved.bin"));
    assert_eq!(status, Some(1), "{stderr}");
    assert!(stderr.contains("missing"), "{stderr}");
    assert!(!stderr.contains("stopped"), "{stderr}");
    // The example as it is saves: its peripherals are built-in types.
    let example = directory.join("example.toml");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/boards/echo.toml"),
        &example,
    )
    .unwrap();
    let (status, stderr) = save(&example, &directory.join("saved.bin"));
    assert_eq!(status, Some(0), "{stderr}");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_board_that_saves_replaces_the_old_snapshot() {
    let file = saving_board("saves");
    let directory = file.parent().unwrap();
    let saved = directory.join("saved.bin");
    let old = vec![0xa5; 1 << 24];
    fs::write(&saved, &old).unwrap();
    // another name for the old file: written in place, it would change too
    let linked = directory.join("linked.bin");
    fs::hard_link(&saved, &linked).unwrap();
    let (status, stderr) = save(&file, &saved);
    assert_eq!(status, Some(0), "{stderr}");
    let snapshot = fs::read(&saved).unwrap();
    assert!(snapshot.len() < old.len());
    assert!(
        fs::read(&linked).unwrap() == old,
        "renamed over, not rewritten"
    );
    assert_eq!(partials(directory), 0);
    let restored = runner(
        &[
            file.to_str().unwrap(),
            "--restore",
            saved.to_str().unwrap(),
            "--seconds",
            "0.001",
        ],
        b"",
    );
    assert!(restored.status.success(), "{restored:?}");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_halt_and_a_wrong_command_have_their_status() {
    // TRAP #0 with nothing at the stack: a double bus fault.
    let directory = board("halt", &rom(&[TRAP]));
    let file = directory.join("echo.toml");
    let halted = runner(&[file.to_str().unwrap(), "--seconds", "0.001"], b"");
    assert_eq!(halted.status.code(), Some(3));
    assert!(
        String::from_utf8(halted.stderr)
            .unwrap()
            .contains("reason=halted")
    );
    // Without an end, or with inf, the run goes on to the halt.
    for arguments in [
        &[file.to_str().unwrap()][..],
        &[file.to_str().unwrap(), "--seconds", "inf"],
    ] {
        assert_eq!(
            runner(arguments, b"").status.code(),
            Some(3),
            "{arguments:?}"
        );
    }
    let unbound = runner(&[file.to_str().unwrap(), "--bind", "nowhere=stdio"], b"");
    assert_eq!(unbound.status.code(), Some(1));
    let usage = runner(&[file.to_str().unwrap(), "--bind", "console=telnet"], b"");
    assert_eq!(usage.status.code(), Some(2));
    let wrong = [
        ["--turn-us", "18446744073709551615"],
        ["--turn-us", "0"],
        ["--seconds", "nan"],
        ["--seconds", "-inf"],
        ["--seconds", "-1"],
        ["--position", "nan"],
        ["--input-after", "-0.5"],
        ["--pace", "fast"],
        ["--pace", "0"],
        ["--pace", "-2"],
    ];
    for [flag, value] in wrong {
        let usage = runner(&[file.to_str().unwrap(), flag, value], b"");
        assert_eq!(usage.status.code(), Some(2), "{flag} {value}");
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn input_to_an_endpoint_that_drives_no_serial_net_is_refused() {
    let directory = board("listening", &echo::image());
    let file = directory.join("echo.toml");
    // the console listens to TXD only
    let text = fs::read_to_string(&file).unwrap();
    let (listening, _) = text.split_once("[[net]]\nname = \"console-in\"").unwrap();
    fs::write(&file, listening).unwrap();
    let file = file.to_str().unwrap();
    let refused = runner(&[file, "--bind", "console=stdio"], b"hello");
    assert_eq!(refused.status.code(), Some(1));
    let stderr = String::from_utf8(refused.stderr).unwrap();
    assert!(
        stderr.contains("--bind console: external.console drives no serial net"),
        "{stderr}"
    );
    assert!(
        stderr.contains("bind it to stdout, file:PATH or null"),
        "{stderr}"
    );
    let written = directory.join("console.txt");
    let output = format!("console=file:{}", written.display());
    for how in ["console=null", "console=stdout", output.as_str()] {
        let ran = runner(&[file, "--bind", how, "--seconds", "0.001"], b"");
        assert!(ran.status.success(), "{how}");
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn stdout_prints_what_an_endpoint_that_only_listens_hears() {
    let directory = board("stdout", &echo::chatter_image());
    let file = directory.join("echo.toml");
    // the console listens to TXD only
    let text = fs::read_to_string(&file).unwrap();
    let (listening, _) = text.split_once("[[net]]\nname = \"console-in\"").unwrap();
    fs::write(&file, listening).unwrap();
    let file = file.to_str().unwrap();
    let ran = runner(
        &[file, "--bind", "console=stdout", "--seconds", "0.01"],
        b"hello",
    );
    let stderr = String::from_utf8(ran.stderr).unwrap();
    assert!(ran.status.success(), "{stderr}");
    assert!(!ran.stdout.is_empty(), "{stderr}");
    assert!(ran.stdout.iter().all(|&byte| byte == b'A'), "{stderr}");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn stdout_leaves_standard_input_to_stdio() {
    let directory = board("stdout-stdio", &echo::image());
    let file = directory.join("echo.toml");
    // a tap on TXD, ahead of the console, so it is bound first
    let text = fs::read_to_string(&file).unwrap();
    let tapped = text.replace(
        "to = [\"external.console\"]",
        "to = [\"external.tap\", \"external.console\"]",
    );
    assert_ne!(tapped, text);
    fs::write(&file, tapped).unwrap();
    let file = file.to_str().unwrap();
    let ran = runner(
        &[
            file,
            "--bind",
            "tap=stdout",
            "--bind",
            "console=stdio",
            "--seconds",
            "0.02",
            "--input-after",
            "0.001",
        ],
        b"hello",
    );
    let stderr = String::from_utf8(ran.stderr).unwrap();
    assert!(ran.status.success(), "{stderr}");
    // the echo twice, the tap's and the console's, a turn's at a time
    let mut echoed = ran.stdout;
    echoed.sort_unstable();
    let mut twice = b"hellohello".to_vec();
    twice.sort_unstable();
    assert_eq!(echoed, twice, "{stderr}");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn standard_input_goes_to_the_first_stdio_on_the_command_line() {
    let directory = board("stdio-first", &echo::image());
    let file = directory.join("echo.toml");
    // `aux`, ahead of the console by name, a tap on TXD that takes no
    // input: given standard input it would be refused
    let text = fs::read_to_string(&file).unwrap();
    let tapped = text.replace(
        "to = [\"external.console\"]",
        "to = [\"external.aux\", \"external.console\"]",
    );
    assert_ne!(tapped, text);
    fs::write(&file, tapped).unwrap();
    let file = file.to_str().unwrap();
    let ran = runner(
        &[
            file,
            "--bind",
            "console=stdio",
            "--bind",
            "aux=stdio",
            "--seconds",
            "0.02",
            "--input-after",
            "0.001",
        ],
        b"hello",
    );
    let stderr = String::from_utf8(ran.stderr).unwrap();
    assert!(ran.status.success(), "{stderr}");
    // the echo twice, the tap's and the console's
    let mut echoed = ran.stdout;
    echoed.sort_unstable();
    let mut twice = b"hellohello".to_vec();
    twice.sort_unstable();
    assert_eq!(echoed, twice, "{stderr}");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn tty_needs_a_terminal_on_standard_input() {
    let directory = board("tty-piped", &echo::image());
    let file = directory.join("echo.toml");
    let refused = runner(&[file.to_str().unwrap(), "--bind", "console=tty"], b"hello");
    let stderr = String::from_utf8(refused.stderr).unwrap();
    assert_eq!(refused.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("standard input is not a terminal"),
        "{stderr}"
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn tty_needs_standard_input_to_itself() {
    let directory = board("tty-taken", &echo::image());
    let file = directory.join("echo.toml");
    let text = fs::read_to_string(&file).unwrap();
    let tapped = text.replace(
        "to = [\"external.console\"]",
        "to = [\"external.aux\", \"external.console\"]",
    );
    fs::write(&file, tapped).unwrap();
    let refused = runner(
        &[
            file.to_str().unwrap(),
            "--bind",
            "console=stdio",
            "--bind",
            "aux=tty",
        ],
        b"",
    );
    let stderr = String::from_utf8(refused.stderr).unwrap();
    assert_eq!(refused.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("standard input is another binding's"),
        "{stderr}"
    );
    fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[test]
fn tty_takes_keystrokes_raw_and_ctrl_bracket_ends_the_run_and_restores_it() {
    let directory = board("tty", &echo::image());
    let saved = directory.join("saved.bin");
    let terminal = openpty(None, None).unwrap();
    let before = tcgetattr(&terminal.slave).unwrap();
    assert!(before.local_flags.contains(LocalFlags::ICANON));
    let mut child = Command::new(env!("CARGO_BIN_EXE_mc683xx-run"))
        .args([
            directory.join("echo.toml").to_str().unwrap(),
            "--bind",
            "console=tty",
            "--seconds",
            "1000",
            "--pace",
            "realtime",
            "--save",
            saved.to_str().unwrap(),
        ])
        .stdin(Stdio::from(terminal.slave.try_clone().unwrap()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    while !line.contains("bound name=console tty") {
        line.clear();
        assert_ne!(stderr.read_line(&mut line).unwrap(), 0, "never bound");
    }
    let raw = tcgetattr(&terminal.slave).unwrap();
    assert!(!raw.local_flags.contains(LocalFlags::ICANON), "raw input");
    assert!(
        !raw.local_flags.contains(LocalFlags::ISIG),
        "keys signal nothing"
    );
    assert!(
        raw.output_flags.contains(OutputFlags::OPOST),
        "output translated"
    );
    let mut master = fs::File::from(terminal.master);
    // no newline: raw input goes in as typed; Ctrl-C is a byte
    master.write_all(b"hi\x03").unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut echoed = [0; 3];
    stdout.read_exact(&mut echoed).unwrap();
    assert_eq!(&echoed, b"hi\x03");
    master.write_all(&[0x1d]).unwrap();
    let mut rest = String::new();
    stderr.read_to_string(&mut rest).unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(4), "{rest}");
    assert!(rest.ends_with("reason=requested\n"), "{rest}");
    assert!(!fs::read(&saved).unwrap().is_empty(), "saved");
    let after = tcgetattr(&terminal.slave).unwrap();
    assert_eq!(after.local_flags, before.local_flags, "restored");
    assert_eq!(after.input_flags, before.input_flags, "restored");
    assert_eq!(after.output_flags, before.output_flags, "restored");
    fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[test]
fn a_pty_nobody_reads_drops_what_it_cannot_take() {
    // About 52 Kbytes at SCBR 1: more than a pty's buffer holds.
    let directory = board("pty", &echo::chatter_image());
    let file = directory.join("echo.toml");
    let arguments = [
        file.to_str().unwrap(),
        "--bind",
        "console=pty",
        "--seconds",
        "1",
    ];
    let output = runner(&arguments, b"");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("mc68331: bound name=console pty="),
        "{stderr}"
    );
    assert!(stderr.ends_with("reason=budget\n"), "{stderr}");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_paced_run_is_the_unpaced_run() {
    let directory = board("paced", &echo::image());
    let file = directory.join("echo.toml");
    let run = |pace: &str| {
        let arguments = [
            file.to_str().unwrap(),
            "--bind",
            "console=stdio",
            "--seconds",
            "0.02",
            "--trace",
            "pins",
            "--input-after",
            "0.001",
            "--pace",
            pace,
        ];
        let output = runner(&arguments, b"hello");
        assert!(output.status.success(), "{pace}");
        (output.stdout, String::from_utf8(output.stderr).unwrap())
    };
    let unpaced = run("unpaced");
    assert_eq!(unpaced.0, b"hello");
    for pace in ["realtime", "4"] {
        assert!(run(pace) == unpaced, "{pace}");
    }
    fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[test]
fn ctrl_c_stops_the_run_cleanly_and_it_still_saves() {
    let directory = board("interrupted", &echo::image());
    let saved = directory.join("saved.bin");
    let mut child = Command::new(env!("CARGO_BIN_EXE_mc683xx-run"))
        .args([
            directory.join("echo.toml").to_str().unwrap(),
            "--seconds",
            "1000",
            "--pace",
            "realtime",
            "--position",
            "0.001",
            "--save",
            saved.to_str().unwrap(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    // the first position comes after the first turn, with the handler set
    let mut line = String::new();
    while !line.contains("position") {
        line.clear();
        assert_ne!(stderr.read_line(&mut line).unwrap(), 0, "no position");
    }
    let pid = Pid::from_raw(i32::try_from(child.id()).unwrap());
    kill(pid, Signal::SIGINT).unwrap();
    let mut rest = String::new();
    stderr.read_to_string(&mut rest).unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(4), "{rest}");
    assert!(rest.ends_with("reason=requested\n"), "{rest}");
    assert!(!fs::read(&saved).unwrap().is_empty(), "saved");
    fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[test]
fn a_second_ctrl_c_ends_the_process_at_once_saving_nothing() {
    let directory = board("interrupted-twice", &echo::image());
    let saved = directory.join("saved.bin");
    // piped input is read whole when it first goes in, at 5 ms: with the
    // pipe held open the run waits there, so the first Ctrl-C cannot end it
    let mut child = Command::new(env!("CARGO_BIN_EXE_mc683xx-run"))
        .args([
            directory.join("echo.toml").to_str().unwrap(),
            "--bind",
            "console=stdio",
            "--input-after",
            "0.005",
            "--seconds",
            "1000",
            "--position",
            "0.001",
            "--save",
            saved.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = child.stdin.take().unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    // the fifth position comes at 5 ms, just before the input is read
    let mut positions = 0;
    let mut line = String::new();
    while positions < 5 {
        line.clear();
        assert_ne!(stderr.read_line(&mut line).unwrap(), 0, "no position");
        positions += usize::from(line.contains("position"));
    }
    let pid = Pid::from_raw(i32::try_from(child.id()).unwrap());
    thread::sleep(Duration::from_millis(200));
    kill(pid, Signal::SIGINT).unwrap();
    thread::sleep(Duration::from_millis(200));
    kill(pid, Signal::SIGINT).unwrap();
    let mut rest = String::new();
    stderr.read_to_string(&mut rest).unwrap();
    let status = child.wait().unwrap();
    drop(input);
    assert_eq!(status.code(), Some(130), "{rest}");
    assert!(!rest.contains("stopped"), "{rest}");
    assert!(!saved.exists(), "nothing saved");
    assert_eq!(partials(&directory), 0);
    fs::remove_dir_all(directory).unwrap();
}

/// gdb's Ctrl-C.
const INTERRUPT: u8 = 0x03;

/// Sends `bytes` to the stub and reads its reply packet's body.
fn exchange(gdb: &mut TcpStream, bytes: &[u8]) -> String {
    gdb.write_all(bytes).unwrap();
    let mut reply = Vec::new();
    let mut byte = [0];
    while !reply.starts_with(b"$") || reply.iter().rev().nth(2) != Some(&b'#') {
        gdb.read_exact(&mut byte).unwrap();
        if byte[0] != b'+' || !reply.is_empty() {
            reply.push(byte[0]);
        }
    }
    String::from_utf8(reply[1..reply.len() - 3].to_vec()).unwrap()
}

/// `body` as a packet.
fn packet(body: &str) -> Vec<u8> {
    let sum = body.bytes().fold(0u8, u8::wrapping_add);
    format!("${body}#{sum:02x}").into_bytes()
}

/// How long the runner has to say where it waits for gdb.
const WAITING: Duration = Duration::from_secs(30);

/// The runner, with `more` arguments and its standard input a pipe the
/// test holds, waiting for gdb on a port it picks, and a connection to
/// it.
fn gdb_session(file: &Path, more: &[&str]) -> (Child, TcpStream) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mc683xx-run"))
        .args([
            file.to_str().unwrap(),
            "--gdb",
            "127.0.0.1:0",
            "--seconds",
            "0.001",
        ])
        .args(more)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut line = String::new();
        stderr.read_line(&mut line).unwrap();
        sender.send(line).unwrap();
        stderr
    });
    let Ok(line) = receiver.recv_timeout(WAITING) else {
        child.kill().unwrap();
        panic!("the runner did not say where it waits for gdb");
    };
    let stderr = reader.join().unwrap();
    let address = line
        .trim()
        .strip_prefix("mc683xx-run: waiting for gdb on ")
        .unwrap_or_else(|| panic!("{line}"))
        .to_owned();
    child.stderr = Some(stderr.into_inner());
    (child, TcpStream::connect(address).unwrap())
}

#[test]
fn gdb_runs_the_board_until_it_detaches() {
    let directory = board("gdb", &echo::image());
    let (child, mut gdb) = gdb_session(&directory.join("echo.toml"), &[]);
    assert!(exchange(&mut gdb, &packet("?")).starts_with("T05"));
    // The program's first word, MOVE.W #imm,(xxx).L, from the ROM.
    assert_eq!(
        exchange(&mut gdb, &packet(&format!("m{START:x},2"))),
        "33fc"
    );
    assert_eq!(exchange(&mut gdb, &packet("s")), "S05");
    gdb.write_all(&packet("c")).unwrap();
    assert_eq!(exchange(&mut gdb, &[INTERRUPT]), "S02");
    assert_eq!(exchange(&mut gdb, &packet("D")), "OK");
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.ends_with("reason=budget\n"), "{stderr}");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_packet_the_stub_cannot_parse_ends_only_the_session() {
    for bad in ["M0,4:zz", "Z0,zz,2"] {
        let directory = board("gdb-bad", &echo::image());
        let (child, mut gdb) = gdb_session(&directory.join("echo.toml"), &[]);
        assert!(exchange(&mut gdb, &packet("?")).starts_with("T05"));
        gdb.write_all(&packet(bad)).unwrap();
        let output = child.wait_with_output().unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{bad}: {stderr}");
        assert!(stderr.contains("mc683xx-run: gdb: "), "{bad}: {stderr}");
        assert!(stderr.ends_with("reason=budget\n"), "{bad}: {stderr}");
        fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn gdb_is_waited_for_before_piped_input_is_read() {
    let directory = board("gdb-stdin", &echo::image());
    let (mut child, mut gdb) = gdb_session(
        &directory.join("echo.toml"),
        &[
            "--bind",
            "console=stdio",
            "--turn-us",
            "100",
            "--input-after",
            "0.0005",
        ],
    );
    assert!(exchange(&mut gdb, &packet("?")).starts_with("T05"));
    assert_eq!(exchange(&mut gdb, &packet("D")), "OK");
    child.stdin.take().unwrap().write_all(b"h").unwrap();
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    assert_eq!(output.stdout, b"h", "{stderr}");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn gdb_kills_the_run() {
    let directory = board("gdb-kill", &echo::image());
    let (child, mut gdb) = gdb_session(&directory.join("echo.toml"), &[]);
    assert!(exchange(&mut gdb, &packet("?")).starts_with("T05"));
    gdb.write_all(&packet("k")).unwrap();
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(4), "{stderr}");
    assert!(stderr.ends_with("reason=requested\n"), "{stderr}");
    fs::remove_dir_all(directory).unwrap();
}

/// Ctrl-C to the runner `child`, and its exit status and the rest of
/// its standard error.
#[cfg(unix)]
fn interrupted(mut child: Child) -> (Option<i32>, String) {
    let pid = Pid::from_raw(i32::try_from(child.id()).unwrap());
    kill(pid, Signal::SIGINT).unwrap();
    let mut rest = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut rest)
        .unwrap();
    (child.wait().unwrap().code(), rest)
}

#[cfg(unix)]
#[test]
fn ctrl_c_under_gdb_stops_the_run_cleanly_and_it_still_saves() {
    let directory = board("gdb-interrupted", &echo::image());
    let saved = directory.join("saved.bin");
    let (child, mut gdb) = gdb_session(
        &directory.join("echo.toml"),
        &["--save", saved.to_str().unwrap()],
    );
    assert!(exchange(&mut gdb, &packet("?")).starts_with("T05"));
    let (status, rest) = interrupted(child);
    assert_eq!(status, Some(4), "{rest}");
    assert!(rest.ends_with("reason=requested\n"), "{rest}");
    assert!(!fs::read(&saved).unwrap().is_empty(), "saved");
    fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[test]
fn ctrl_c_while_waiting_for_gdb_stops_the_run_cleanly() {
    let directory = board("gdb-waiting", &echo::image());
    let saved = directory.join("saved.bin");
    let mut child = Command::new(env!("CARGO_BIN_EXE_mc683xx-run"))
        .args([
            directory.join("echo.toml").to_str().unwrap(),
            "--gdb",
            "127.0.0.1:0",
            "--save",
            saved.to_str().unwrap(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    assert!(line.contains("waiting for gdb"), "{line}");
    child.stderr = Some(stderr.into_inner());
    let (status, rest) = interrupted(child);
    assert_eq!(status, Some(4), "{rest}");
    assert!(rest.ends_with("reason=requested\n"), "{rest}");
    assert!(!fs::read(&saved).unwrap().is_empty(), "saved");
    fs::remove_dir_all(directory).unwrap();
}

/// One request to the virtual BDM cable and its reply, framed as the
/// pod protocol's stream framing: a length byte, then the packet.
#[cfg(unix)]
fn bdm(cable: &mut UnixStream, request: &[u8]) -> Vec<u8> {
    let mut message = vec![u8::try_from(request.len()).unwrap()];
    message.extend_from_slice(request);
    cable.write_all(&message).unwrap();
    let mut length = [0];
    cable.read_exact(&mut length).unwrap();
    let mut reply = vec![0; usize::from(length[0])];
    cable.read_exact(&mut reply).unwrap();
    reply
}

/// The runner with `--bdm` on a Unix socket in `directory`, and a
/// connection to it.
#[cfg(unix)]
fn bdm_session(file: &Path, directory: &Path) -> (Child, UnixStream) {
    let socket = directory.join("bdm.sock");
    let _ = fs::remove_file(&socket);
    let address = format!("unix:{}", socket.display());
    let mut child = Command::new(env!("CARGO_BIN_EXE_mc683xx-run"))
        .args([file.to_str().unwrap(), "--bdm", &address])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut line = String::new();
        stderr.read_line(&mut line).unwrap();
        sender.send(line).unwrap();
        stderr
    });
    let Ok(line) = receiver.recv_timeout(WAITING) else {
        child.kill().unwrap();
        panic!("the runner did not say where it answers BDM");
    };
    child.stderr = Some(reader.join().unwrap().into_inner());
    assert_eq!(line.trim(), format!("mc683xx-run: BDM on {address}"));
    (child, UnixStream::connect(&socket).unwrap())
}

/// `interrupted`, failing if the runner has not ended within
/// [`WAITING`].
#[cfg(unix)]
fn interrupted_promptly(child: Child) -> Option<i32> {
    let pid = Pid::from_raw(i32::try_from(child.id()).unwrap());
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || sender.send(interrupted(child).0).unwrap());
    let Ok(status) = receiver.recv_timeout(WAITING) else {
        kill(pid, Signal::SIGKILL).unwrap();
        panic!("the runner did not end on Ctrl-C");
    };
    status
}

#[cfg(unix)]
#[test]
fn a_bdm_client_that_does_not_read_its_replies_cannot_wedge_the_runner() {
    let directory = board("bdm-flood", &echo::image());
    let (child, mut cable) = bdm_session(&directory.join("echo.toml"), &directory);
    // ResetHalt, so the unpaced runner waits on the cable
    assert_eq!(bdm(&mut cable, &[0x03]), [0, 1, 0, 0, 0, 0]);
    cable.set_nonblocking(true).unwrap();
    // Hellos until the socket stays full: the runner reads no more
    let mut sent = 0;
    let mut full = 0;
    while full < 50 {
        match cable.write(&[1, 0x0e]) {
            Ok(2) => {
                sent += 1;
                full = 0;
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                full += 1;
                thread::sleep(Duration::from_millis(10));
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(sent > 100, "{sent} Hellos");
    assert!(interrupted_promptly(child).is_some());
    fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[test]
fn a_bdm_message_s_length_out_of_range_closes_the_connection() {
    let directory = board("bdm-length", &echo::image());
    let (child, mut cable) = bdm_session(&directory.join("echo.toml"), &directory);
    for length in [0, 65] {
        cable.write_all(&[length, 0x0e]).unwrap();
        assert_eq!(cable.read(&mut [0]).unwrap(), 0, "length {length}");
        cable = UnixStream::connect(directory.join("bdm.sock")).unwrap();
    }
    assert_eq!(bdm(&mut cable, &[0x0e]), [0, 2, 0, 0, 0]);
    assert!(interrupted_promptly(child).is_some());
    fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[test]
fn a_bdm_debugger_drives_the_board_over_the_virtual_cable() {
    let image = echo::image();
    let directory = board("bdm", &image);
    let (child, mut cable) = bdm_session(&directory.join("echo.toml"), &directory);
    // Hello: version 2
    assert_eq!(bdm(&mut cable, &[0x0e]), [0, 2, 0, 0, 0]);
    // Pins: running, FREEZE low, RESET and DS high, 5000 mV, kind 0
    assert_eq!(
        bdm(&mut cable, &[0x02]),
        [0, 0b110, 0x88, 0x13, 0, 0, 0, 0, 0]
    );
    // a register needs FREEZE
    assert_eq!(bdm(&mut cable, &[0x07, 0]), [3]);
    // ResetHalt: a fresh breakpoint entry, ATEMP 0
    assert_eq!(bdm(&mut cable, &[0x03]), [0, 1, 0, 0, 0, 0]);
    assert_eq!(bdm(&mut cable, &[0x02])[1] & 1, 1, "FREEZE high");
    let start = START.to_le_bytes();
    assert_eq!(
        bdm(&mut cable, &[0x09, 0x00]),
        [0, start[0], start[1], start[2], start[3]],
        "RPC at the reset PC"
    );
    // read the first word of the program through SFC = supervisor program
    assert_eq!(bdm(&mut cable, &[0x0a, 0x0e, 6, 0, 0, 0]), [0]);
    let at = START as usize;
    assert_eq!(
        bdm(
            &mut cable,
            &[0x0b, start[0], start[1], start[2], start[3], 1, 2]
        ),
        [0, image[at], image[at + 1]]
    );
    // a word at an odd address is the target's address error
    assert_eq!(bdm(&mut cable, &[0x0b, 1, 4, 0, 0, 1, 2]), [4]);
    assert_eq!(bdm(&mut cable, &[0x07, 16]), [1], "no register 16");
    // a raw RSREG of RPC (CPU32RM 7.2.8.6): "not ready" with the
    // command, then RPC, high word first, one u32 per word
    let transfer = bdm(&mut cable, &[0x01, 0x80, 0x25, 0, 0, 0, 0]);
    let expected: Vec<u8> = [0x1_0000, START >> 16, START & 0xffff]
        .iter()
        .flat_map(|word: &u32| word.to_le_bytes())
        .collect();
    assert_eq!(transfer[0], 0);
    assert_eq!(transfer[1..], expected);
    assert_eq!(bdm(&mut cable, &[0x01]), [1], "no words");
    assert_eq!(bdm(&mut cable, &[0x01, 0]), [1], "half a word");
    let mut sixteen = vec![0x01];
    sixteen.extend([0; 32]);
    assert_eq!(bdm(&mut cable, &sixteen), [1], "16 words");
    // a second client is closed, once the first has had its grace
    let mut second = UnixStream::connect(directory.join("bdm.sock")).unwrap();
    assert_eq!(bdm(&mut cable, &[0x0e]), [0, 2, 0, 0, 0]);
    assert_eq!(second.read(&mut [0]).unwrap(), 0);
    // a client that has just closed gives way to the next at once: the
    // next connects before the old one closes
    for _ in 0..3 {
        cable = UnixStream::connect(directory.join("bdm.sock")).unwrap();
        assert_eq!(bdm(&mut cable, &[0x0e]), [0, 2, 0, 0, 0]);
    }
    // clients that come and go before the server looks: each that has
    // closed gives way to the next
    drop(cable);
    drop(UnixStream::connect(directory.join("bdm.sock")).unwrap());
    cable = UnixStream::connect(directory.join("bdm.sock")).unwrap();
    assert_eq!(bdm(&mut cable, &[0x0e]), [0, 2, 0, 0, 0]);
    // Go, then Halt
    assert_eq!(bdm(&mut cable, &[0x05]), [0]);
    assert_eq!(bdm(&mut cable, &[0x02])[1] & 1, 0, "running");
    assert_eq!(bdm(&mut cable, &[0x04]), [0, 1, 0, 0, 0, 0]);
    assert_eq!(bdm(&mut cable, &[0x04]), [0, 1, 0, 0, 0, 0], "and again");
    let (status, _) = interrupted(child);
    assert!(status.is_some());
}

/// The longest a read or write on the cable may wait.
#[cfg(unix)]
const CABLE_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest the test's gdb sessions over the cable may take.
#[cfg(unix)]
const SESSIONS_DEADLINE: Duration = Duration::from_secs(60);
/// The most bytes one memory request to the cable carries.
#[cfg(unix)]
const CABLE_MEMORY_BYTES: usize = 56;
/// PC's index in a `g` packet.
#[cfg(unix)]
const PC: usize = 17;
/// Registers in the core feature of a `G` packet: D0-D7, A0-A7, SR, PC.
#[cfg(unix)]
const CORE_REGISTERS: usize = 18;
/// The program gdb writes into RAM (CPU32RM 4): `moveq #5,d0`, then
/// `addq.l #1,d0` and `bra.s` back to it, for ever.
#[cfg(unix)]
const RAM_PROGRAM: [u16; 3] = [0x7005, 0x5280, 0x60fc];
/// The `addq`'s address, where gdb's breakpoint goes.
#[cfg(unix)]
const RAM_BREAKPOINT: u32 = RAM_BASE + 2;

/// The virtual cable's refusal of a request.
#[cfg(unix)]
#[derive(Debug, thiserror::Error)]
enum CableError {
    /// The reply's status, not Ok (the protocol's reply table).
    #[error("the cable answered status {0}")]
    Status(u8),
    /// The protocol has no step request.
    #[error("the cable cannot step")]
    NoStep,
}

/// The virtual cable as a pod's host drives it: a `Cpu32Target` whose
/// every call is the protocol's request (`crates/mc68331-run/src/bdm.rs`).
#[cfg(unix)]
struct Cable(UnixStream);

#[cfg(unix)]
impl Cable {
    /// The body of the reply to `request`.
    fn request(&mut self, request: &[u8]) -> Result<Vec<u8>, CableError> {
        let reply = bdm(&mut self.0, request);
        match reply.split_first() {
            Some((0, body)) => Ok(body.to_vec()),
            Some((&status, _)) => Err(CableError::Status(status)),
            None => Err(CableError::Status(u8::MAX)),
        }
    }

    /// The entry an ATEMP field (its kind, then ATEMP) reports, if the
    /// CPU is in background mode.
    fn entry(field: &[u8]) -> Option<Entry> {
        let atemp = u32::from_le_bytes([field[1], field[2], field[3], field[4]]);
        (field[0] != 0).then(|| Entry::from_atemp(atemp))
    }

    /// `access` with `register`, SFC or DFC, holding `function_code`,
    /// and then as it was.
    fn in_space<R>(
        &mut self,
        register: SystemRegister,
        function_code: FunctionCode,
        access: impl FnOnce(&mut Self) -> Result<R, CableError>,
    ) -> Result<R, CableError> {
        let held = self.read_system_register(register)?;
        self.write_system_register(register, u32::from(function_code.bits()))?;
        let result = access(self);
        self.write_system_register(register, held)?;
        result
    }

    /// A memory request's header: its code, `address` and `size`.
    fn memory_request(code: u8, address: Address, size: Size) -> Vec<u8> {
        let mut request = vec![code];
        request.extend_from_slice(&address.0.to_le_bytes());
        request.push(size.field() as u8);
        request
    }
}

#[cfg(unix)]
impl Cpu32Target for Cable {
    type Error = CableError;

    fn reset_halted(&mut self) -> Result<Entry, CableError> {
        let field = self.request(&[0x03])?;
        Ok(Self::entry(&field).unwrap_or(Entry::Breakpoint))
    }

    fn halt(&mut self) -> Result<Entry, CableError> {
        let field = self.request(&[0x04])?;
        Ok(Self::entry(&field).unwrap_or(Entry::Breakpoint))
    }

    fn halted(&mut self) -> Result<Option<Entry>, CableError> {
        let pins = self.request(&[0x02])?;
        Ok(Self::entry(&pins[3..]))
    }

    fn go(&mut self) -> Result<(), CableError> {
        self.request(&[0x05]).map(drop)
    }

    fn step(&mut self) -> Result<Entry, CableError> {
        Err(CableError::NoStep)
    }

    fn read_register(&mut self, register: Register) -> Result<u32, CableError> {
        let value = self.request(&[0x07, register.code() as u8])?;
        Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
    }

    fn write_register(&mut self, register: Register, value: u32) -> Result<(), CableError> {
        let mut request = vec![0x08, register.code() as u8];
        request.extend_from_slice(&value.to_le_bytes());
        self.request(&request).map(drop)
    }

    fn read_system_register(&mut self, register: SystemRegister) -> Result<u32, CableError> {
        let value = self.request(&[0x09, register.code() as u8])?;
        Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
    }

    fn write_system_register(
        &mut self,
        register: SystemRegister,
        value: u32,
    ) -> Result<(), CableError> {
        let mut request = vec![0x0a, register.code() as u8];
        request.extend_from_slice(&value.to_le_bytes());
        self.request(&request).map(drop)
    }

    fn read_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        size: Size,
        data: &mut [u8],
    ) -> Result<(), CableError> {
        self.in_space(SystemRegister::Sfc, function_code, |cable| {
            for (index, chunk) in data.chunks_mut(CABLE_MEMORY_BYTES).enumerate() {
                let at = address.offset((index * CABLE_MEMORY_BYTES) as u32);
                let mut request = Self::memory_request(0x0b, at, size);
                request.push(chunk.len() as u8);
                chunk.copy_from_slice(&cable.request(&request)?);
            }
            Ok(())
        })
    }

    fn write_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        size: Size,
        data: &[u8],
    ) -> Result<(), CableError> {
        self.in_space(SystemRegister::Dfc, function_code, |cable| {
            for (index, chunk) in data.chunks(CABLE_MEMORY_BYTES).enumerate() {
                let at = address.offset((index * CABLE_MEMORY_BYTES) as u32);
                let mut request = Self::memory_request(0x0c, at, size);
                request.extend_from_slice(chunk);
                cable.request(&request)?;
            }
            Ok(())
        })
    }
}

/// gdb's `script` served over the cable through `Bdm`: how the session
/// ended, and the stub's replies.
#[cfg(unix)]
fn gdb_over(
    cable: &mut Cable,
    script: Vec<Send>,
) -> (
    Result<Ending, SessionError<CableError, io::Error>>,
    Vec<String>,
) {
    let output = Rc::new(RefCell::new(Vec::new()));
    let ending = serve(&mut Bdm(cable), Wire::new(script, &output));
    let replies = replies(&output.borrow());
    (ending, replies)
}

#[cfg(unix)]
#[test]
fn gdb_drives_the_board_over_the_virtual_cable_as_over_a_pod() {
    let directory = board("gdb-bdm", &echo::image());
    let (mut child, cable) = bdm_session(&directory.join("echo.toml"), &directory);
    cable.set_read_timeout(Some(CABLE_TIMEOUT)).unwrap();
    cable.set_write_timeout(Some(CABLE_TIMEOUT)).unwrap();
    // the sessions in a thread of their own, so a stub that never
    // answers fails the test, the runner ended, rather than hang it
    let (sender, receiver) = mpsc::channel();
    let sessions = thread::spawn(move || {
        gdb_sessions_over(Cable(cable));
        sender.send(()).unwrap();
    });
    match receiver.recv_timeout(SESSIONS_DEADLINE) {
        Ok(()) => sessions.join().unwrap(),
        Err(RecvTimeoutError::Timeout) => {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("the gdb sessions over the cable did not end in {SESSIONS_DEADLINE:?}");
        }
        Err(RecvTimeoutError::Disconnected) => {
            child.kill().unwrap();
            child.wait().unwrap();
            match sessions.join() {
                Err(panicked) => panic::resume_unwind(panicked),
                Ok(()) => unreachable!("the sessions ended without saying so"),
            }
        }
    }
    let (status, _) = interrupted(child);
    assert!(status.is_some());
    fs::remove_dir_all(directory).unwrap();
}

/// The test's gdb sessions over `cable`, a pod's host's.
#[cfg(unix)]
fn gdb_sessions_over(mut cable: Cable) {
    // BDM is enabled by a reset with BKPT held, as a pod resets
    cable.reset_halted().unwrap();
    // the firmware runs, and Ctrl-C halts it
    let (ending, replies) = gdb_over(
        &mut cable,
        vec![
            wire::packet("c"),
            Send::Now(vec![INTERRUPT]),
            wire::packet("k"),
        ],
    );
    assert_eq!(ending.unwrap(), Ending::Killed);
    assert_eq!(replies[0], "S02");
    // until its prologue has mapped the RAM
    let mut word = [0; 2];
    for _ in 0..100 {
        let mapped = cable.read_memory(
            Address(RAM_BASE),
            FunctionCode::SUPERVISOR_DATA,
            Size::Word,
            &mut word,
        );
        if mapped.is_ok() {
            break;
        }
        cable.go().unwrap();
        thread::sleep(Duration::from_millis(10));
        cable.halt().unwrap();
    }
    // gdb writes a program into RAM, sets PC there, and breaks in it
    let mut core: Vec<u32> = (0..16)
        .map(|code| {
            cable
                .read_register(Register::from_code(code).unwrap())
                .unwrap()
        })
        .collect();
    core.push(cable.read_system_register(SystemRegister::Sr).unwrap());
    core.push(RAM_BASE);
    assert_eq!(core.len(), CORE_REGISTERS);
    let core: String = core.iter().map(|value| format!("{value:08x}")).collect();
    let program: String = bytes(&RAM_PROGRAM)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let (ending, replies) = gdb_over(
        &mut cable,
        vec![
            wire::packet(&format!("M{RAM_BASE:x},6:{program}")),
            wire::packet(&format!("G{core}")),
            wire::packet(&format!("Z0,{RAM_BREAKPOINT:x},2")),
            wire::packet("c"),
            wire::packet("g"),
            wire::packet(&format!("z0,{RAM_BREAKPOINT:x},2")),
            wire::packet(&format!("m{RAM_BREAKPOINT:x},2")),
            wire::packet(&format!("qRcmd,{}", hex("fc"))),
            wire::packet("D"),
        ],
    );
    assert_eq!(ending.unwrap(), Ending::Detached);
    assert_eq!(replies[..3], ["OK", "OK", "OK"]);
    assert!(replies[3].starts_with("T05") && replies[3].contains("swbreak"));
    let stopped = registers(&replies[4]);
    assert_eq!(stopped[PC], RAM_BREAKPOINT, "on the breakpoint");
    assert_eq!(stopped[0], 5, "D0 after the moveq");
    assert_eq!(replies[5], "OK");
    assert_eq!(replies[6], "5280", "BGND removed");
    assert!(replies.contains(&format!("O{}", hex("fc 5\n"))));
    // detached, it runs on round the loop
    assert_eq!(cable.halted().unwrap(), None);
    let mut ran_on = false;
    for _ in 0..100 {
        thread::sleep(Duration::from_millis(10));
        cable.halt().unwrap();
        ran_on = cable.read_register(Register::D0).unwrap() > 5;
        if ran_on {
            break;
        }
        cable.go().unwrap();
    }
    assert!(ran_on, "D0 counts on");
    cable
        .read_memory(
            Address(RAM_BREAKPOINT),
            FunctionCode::SUPERVISOR_DATA,
            Size::Word,
            &mut word,
        )
        .unwrap();
    assert_eq!(word, [0x52, 0x80]);
}
