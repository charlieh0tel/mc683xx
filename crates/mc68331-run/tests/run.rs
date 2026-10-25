//! The runner end to end: `examples/boards/echo.toml` with its
//! firmware, which echoes the SCI, its input piped in; and its exit status when
//! the CPU halts or the command is wrong; and a gdb session with it
//! (`--gdb`), over TCP with raw packets.

use std::env;
use std::fs;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::io::Write;
use std::net::TcpStream;
use std::path::Path;
use std::path::PathBuf;
use std::process;
use std::process::Child;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use mc683xx_examples::echo;
use mc683xx_examples::firmware::START;
use mc683xx_examples::firmware::rom;
#[cfg(unix)]
use nix::sys::signal::Signal;
#[cfg(unix)]
use nix::sys::signal::kill;
#[cfg(unix)]
use nix::unistd::Pid;

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
    child.stdin.take().unwrap().write_all(input).unwrap();
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
