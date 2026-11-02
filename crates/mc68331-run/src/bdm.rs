//! `--bdm`: a virtual BDM cable.  The runner answers on a socket as the
//! m683xx BDM pod answers on USB (its host-pod protocol, version 2:
//! github.com/charlieh0tel/m683xx-pod, `docs/usb-protocol.md`), so a
//! debugger that drives a 68331 through the pod drives the board the
//! same way, through the chip's [`Cpu32Target`].
//!
//! One client at a time; another connection is closed once the client
//! has had 100 ms to go, polls going on meanwhile.  The server is polled between the runner's turns and, while the CPU is in
//! background mode on an unpaced run, waited on, so board time stands
//! still between requests (PLAN.md, Background debug mode).

use std::io;
use std::io::ErrorKind;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
#[cfg(unix)]
use std::os::unix::net::UnixListener;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use mc683xx_mc68331::chip::Mc68331;
use mc683xx_mc68331::debug::TargetError;
use mc683xx_target::Cpu32Target;
use mc683xx_target::address::Address;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::register::Register;
use mc683xx_target::register::SystemRegister;
use mc683xx_target::size::Size;

/// The protocol version Hello answers.
const PROTOCOL_VERSION: u32 = 2;
/// The longest packet, a USB full-speed bulk packet.
const MAX_PACKET: usize = 64;
/// The most words one Transfer carries.
const MAX_TRANSFER_WORDS: usize = 15;
/// The most bytes one memory request carries.
const MAX_MEMORY_BYTES: usize = 56;
/// What Pins reports for VDD, in millivolts: no supply to measure.
const VDD_MILLIVOLTS: u16 = 5000;
/// `SetClock`'s fastest DSCLK.
const MAX_DSCLK_HZ: u32 = 4_000_000;
/// Pins' FREEZE bit.
const PIN_FREEZE: u8 = 1 << 0;
/// Pins' RESET bit: high, the runner holds no reset.
const PIN_RESET: u8 = 1 << 1;
/// Pins' DS bit: high between cycles.
const PIN_DS: u8 = 1 << 2;
/// How long a wait sleeps between looks at the socket and Ctrl-C.
const WAIT_TICK: Duration = Duration::from_millis(10);
/// How long a new connection waits for the current client's end of
/// file before it is refused: a client that has just closed is not yet
/// seen gone (the protocol's Transport).
const CLOSE_GRACE: Duration = Duration::from_millis(100);
/// The most bytes read and not yet answered: two whole messages.  The
/// server reads no more until it answers, so a client that floods it
/// waits on its own socket.
const MAX_PENDING: usize = 2 * (1 + MAX_PACKET);
/// The most bytes of replies the client has not yet taken before the
/// server answers no more: a client that does not read waits.
const MAX_OUTGOING: usize = 4 * (1 + MAX_PACKET);
/// The most requests one poll answers, so a client cannot keep the
/// runner from its turn.
const MAX_ANSWERS: usize = 32;

/// A request's code (the protocol's request table).
mod code {
    pub(super) const TRANSFER: u8 = 0x01;
    pub(super) const PINS: u8 = 0x02;
    pub(super) const RESET_HALT: u8 = 0x03;
    pub(super) const HALT: u8 = 0x04;
    pub(super) const GO: u8 = 0x05;
    pub(super) const SET_CLOCK: u8 = 0x06;
    pub(super) const READ_REGISTER: u8 = 0x07;
    pub(super) const WRITE_REGISTER: u8 = 0x08;
    pub(super) const READ_SYSTEM_REGISTER: u8 = 0x09;
    pub(super) const WRITE_SYSTEM_REGISTER: u8 = 0x0A;
    pub(super) const READ_MEMORY: u8 = 0x0B;
    pub(super) const WRITE_MEMORY: u8 = 0x0C;
    pub(super) const CALL: u8 = 0x0D;
    pub(super) const HELLO: u8 = 0x0E;
}

/// A reply's status byte (the protocol's reply table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// Done; the body is the result.
    Ok = 0,
    /// Not a request the protocol parses.
    BadRequest = 1,
    /// FREEZE did not assert, or is not asserted for a request that
    /// needs it.
    NoFreeze = 3,
    /// The access ended in a bus or address error.
    BusError = 4,
}

/// The ATEMP field's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Not in background mode.
    Running = 0,
    /// Read first after entry: the entry source.
    Fresh = 1,
    /// Not seen entered: not an entry source.
    Stale = 2,
}

/// A parsed request.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Request {
    /// Raw 17-bit exchanges, the words sent.
    Transfer(Vec<u16>),
    /// The pins and the kept ATEMP.
    Pins,
    /// Reset into background mode.
    ResetHalt,
    /// Stop in background mode.
    Halt,
    /// Resume at RPC.
    Go,
    /// Set DSCLK's rate, in hertz.
    SetClock(u32),
    /// Read a data or address register.
    ReadRegister(Register),
    /// Write one.
    WriteRegister(Register, u32),
    /// Read a system register.
    ReadSystemRegister(SystemRegister),
    /// Write one.
    WriteSystemRegister(SystemRegister, u32),
    /// Read `length` bytes through SFC.
    ReadMemory(Address, Size, usize),
    /// Write bytes through DFC.
    WriteMemory(Address, Size, Vec<u8>),
    /// Stack RPC and resume at the address.
    Call(Address),
    /// The protocol version.
    Hello,
}

/// A little-endian u32 at `at`.
fn long(body: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]])
}

/// A size byte: 0 byte, 1 word, 2 long, and nothing else.
fn size(byte: u8) -> Option<Size> {
    Size::from_field(u16::from(byte))
}

/// `length` bytes of `size` accesses: 1 to 56, whole accesses.
fn whole(length: usize, size: Size) -> bool {
    (1..=MAX_MEMORY_BYTES).contains(&length) && length.is_multiple_of(size.bytes() as usize)
}

impl Request {
    /// The request `packet` carries, or `None` for `BadRequest`: an empty
    /// packet, an unknown code, a body of another length than the code
    /// takes, or a field out of range.
    fn parse(packet: &[u8]) -> Option<Self> {
        let (&code, body) = packet.split_first()?;
        let register = |byte: u8| Register::from_code(u16::from(byte));
        let system = |byte: u8| SystemRegister::from_code(u16::from(byte));
        match (code, body.len()) {
            (code::TRANSFER, length)
                if length.is_multiple_of(2) && (2..=2 * MAX_TRANSFER_WORDS).contains(&length) =>
            {
                Some(Self::Transfer(
                    body.chunks(2)
                        .map(|word| u16::from_le_bytes([word[0], word[1]]))
                        .collect(),
                ))
            }
            (code::PINS, 0) => Some(Self::Pins),
            (code::RESET_HALT, 0) => Some(Self::ResetHalt),
            (code::HALT, 0) => Some(Self::Halt),
            (code::GO, 0) => Some(Self::Go),
            (code::SET_CLOCK, 4) => Some(Self::SetClock(long(body, 0))),
            (code::READ_REGISTER, 1) => register(body[0]).map(Self::ReadRegister),
            (code::WRITE_REGISTER, 5) => {
                register(body[0]).map(|register| Self::WriteRegister(register, long(body, 1)))
            }
            (code::READ_SYSTEM_REGISTER, 1) => system(body[0]).map(Self::ReadSystemRegister),
            (code::WRITE_SYSTEM_REGISTER, 5) => {
                system(body[0]).map(|register| Self::WriteSystemRegister(register, long(body, 1)))
            }
            (code::READ_MEMORY, 6) => {
                let size = size(body[4])?;
                let length = usize::from(body[5]);
                whole(length, size).then(|| Self::ReadMemory(Address(long(body, 0)), size, length))
            }
            (code::WRITE_MEMORY, 6..) => {
                let size = size(body[4])?;
                let data = &body[5..];
                whole(data.len(), size)
                    .then(|| Self::WriteMemory(Address(long(body, 0)), size, data.to_vec()))
            }
            (code::CALL, 4) => Some(Self::Call(Address(long(body, 0)))),
            (code::HELLO, 0) => Some(Self::Hello),
            _ => None,
        }
    }
}

/// A reply: its status, and its body for [`Status::Ok`].
fn reply(status: Status, body: &[u8]) -> Vec<u8> {
    let mut packet = vec![status as u8];
    if status == Status::Ok {
        packet.extend_from_slice(body);
    }
    packet
}

/// The status a target error answers.
const fn status(error: TargetError) -> Status {
    match error {
        TargetError::Access { .. } => Status::BusError,
        TargetError::Length { .. } => Status::BadRequest,
        _ => Status::NoFreeze,
    }
}

/// A listening socket.
enum Listener {
    /// TCP, HOST:PORT.
    Tcp(TcpListener),
    /// A Unix socket, `unix:PATH`.
    #[cfg(unix)]
    Unix(UnixListener),
}

/// A client's connection.
enum Stream {
    /// TCP.
    Tcp(TcpStream),
    /// A Unix socket.
    #[cfg(unix)]
    Unix(UnixStream),
}

impl Listener {
    /// A listener on `address`, `unix:PATH` or HOST:PORT, not blocking.
    fn bind(address: &str) -> io::Result<Self> {
        let listener = match address.strip_prefix("unix:") {
            #[cfg(unix)]
            Some(path) => Self::Unix(UnixListener::bind(path)?),
            #[cfg(not(unix))]
            Some(_) => {
                return Err(io::Error::new(
                    ErrorKind::Unsupported,
                    "Unix sockets need a Unix host",
                ));
            }
            None => Self::Tcp(TcpListener::bind(address)?),
        };
        match &listener {
            Self::Tcp(listener) => listener.set_nonblocking(true)?,
            #[cfg(unix)]
            Self::Unix(listener) => listener.set_nonblocking(true)?,
        }
        Ok(listener)
    }

    /// Where it listens, as standard error names it.
    fn local(&self) -> String {
        match self {
            Self::Tcp(listener) => listener
                .local_addr()
                .map_or_else(|error| error.to_string(), |address| address.to_string()),
            #[cfg(unix)]
            Self::Unix(listener) => listener.local_addr().map_or_else(
                |error| error.to_string(),
                |address| {
                    format!(
                        "unix:{}",
                        address
                            .as_pathname()
                            .unwrap_or_else(|| "?".as_ref())
                            .display()
                    )
                },
            ),
        }
    }

    /// A connection waiting, if one is, not blocking.
    fn accept(&self) -> io::Result<Option<Stream>> {
        let accepted = match self {
            Self::Tcp(listener) => listener.accept().map(|(stream, _)| Stream::Tcp(stream)),
            #[cfg(unix)]
            Self::Unix(listener) => listener.accept().map(|(stream, _)| Stream::Unix(stream)),
        };
        match accepted {
            Ok(stream) => {
                stream.set_nonblocking(true)?;
                Ok(Some(stream))
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error),
        }
    }
}

impl Stream {
    /// Sets whether reads block.
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.set_nonblocking(nonblocking),
            #[cfg(unix)]
            Self::Unix(stream) => stream.set_nonblocking(nonblocking),
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buffer),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.write(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.flush(),
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
        }
    }
}

/// The virtual cable's server.
pub(crate) struct Server {
    /// Where clients connect.
    listener: Listener,
    /// The client, while one is connected.
    client: Option<Stream>,
    /// Bytes read and not yet answered, at most [`MAX_PENDING`].
    pending: Vec<u8>,
    /// Replies the client has not yet taken.
    outgoing: Vec<u8>,
    /// A connection waiting for the client's end of file, and when it
    /// is refused.  Polls go on meanwhile; others wait their turn.
    waiting: Option<(Stream, Instant)>,
    /// ATEMP as the server started, if the CPU was in background mode
    /// then: an entry it did not see, reported stale with this value
    /// until a GO, CALL or reset.
    stale: Option<u32>,
}

impl Server {
    /// A server on `address`, `unix:PATH` or HOST:PORT, for `chip`.
    pub(crate) fn bind(address: &str, chip: &mut Mc68331) -> io::Result<Self> {
        let listener = Listener::bind(address)?;
        eprintln!("mc683xx-run: BDM on {}", listener.local());
        Ok(Self {
            listener,
            client: None,
            pending: Vec::new(),
            outgoing: Vec::new(),
            waiting: None,
            stale: Cpu32Target::halted(chip)
                .ok()
                .flatten()
                .map(|_| chip.cpu().atemp()),
        })
    }

    /// Hands the cable to a new connection if the client has gone, and
    /// answers up to [`MAX_ANSWERS`] whole requests the client has sent,
    /// not blocking.  Whether it answered one.
    pub(crate) fn poll(&mut self, chip: &mut Mc68331) -> bool {
        self.hand_off();
        let mut answers = 0;
        while answers < MAX_ANSWERS && self.flush() {
            match self.message() {
                Ok(Some(packet)) => {
                    let answer = self.answer(chip, &packet);
                    self.send(&answer);
                    answers += 1;
                }
                Ok(None) => {
                    if !self.receive() {
                        break;
                    }
                }
                Err(()) => {
                    // a broken stream
                    self.drop_client();
                    break;
                }
            }
        }
        self.flush();
        answers > 0
    }

    /// The first whole message pending, taken, if there is one; an
    /// error if its length byte is out of range.
    fn message(&mut self) -> Result<Option<Vec<u8>>, ()> {
        let Some(&length) = self.pending.first() else {
            return Ok(None);
        };
        let length = usize::from(length);
        if length == 0 || length > MAX_PACKET {
            return Err(());
        }
        if self.pending.len() <= length {
            return Ok(None);
        }
        Ok(Some(self.pending.drain(..=length).skip(1).collect()))
    }

    /// Reads what the client has sent, up to [`MAX_PENDING`] pending.
    /// Whether it read anything; the client is dropped if it ended.
    fn receive(&mut self) -> bool {
        let mut buffer = [0; MAX_PENDING];
        loop {
            let room = MAX_PENDING - self.pending.len();
            let Some(client) = &mut self.client else {
                return false;
            };
            if room == 0 {
                return false;
            }
            match client.read(&mut buffer[..room]) {
                Ok(0) => {
                    self.drop_client();
                    return false;
                }
                Ok(read) => {
                    self.pending.extend_from_slice(&buffer[..read]);
                    return true;
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => return false,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => {
                    self.drop_client();
                    return false;
                }
            }
        }
    }

    /// Waits for a request and answers it, or until `interrupted`.
    pub(crate) fn wait(&mut self, chip: &mut Mc68331, interrupted: &AtomicBool) {
        while !interrupted.load(Ordering::SeqCst) {
            if self.poll(chip) {
                return;
            }
            thread::sleep(WAIT_TICK);
        }
    }

    /// Takes one connection waiting, if none is waiting already, and
    /// gives it the cable once the client has gone; it is refused,
    /// closed, if the client is still there [`CLOSE_GRACE`] after it
    /// came.  What the client sends meanwhile is kept and answered.
    fn hand_off(&mut self) {
        if self.waiting.is_none()
            && let Ok(Some(stream)) = self.listener.accept()
        {
            self.waiting = Some((stream, Instant::now() + CLOSE_GRACE));
        }
        let Some((_, refused)) = &self.waiting else {
            return;
        };
        let refused = *refused;
        // its end of file, if it has closed
        self.receive();
        if self.client.is_none() {
            self.client = self.waiting.take().map(|(stream, _)| stream);
        } else if Instant::now() >= refused {
            self.waiting = None;
        }
    }

    /// Closes the client's connection.
    fn drop_client(&mut self) {
        self.client = None;
        self.pending.clear();
        self.outgoing.clear();
    }

    /// Queues `packet` with its length byte for the client.
    fn send(&mut self, packet: &[u8]) {
        if self.client.is_some() {
            self.outgoing.push(packet.len() as u8);
            self.outgoing.extend_from_slice(packet);
        }
    }

    /// Writes what replies the client will take, not blocking.  Whether
    /// there is room for another, under [`MAX_OUTGOING`] still queued;
    /// false, too, with no client.
    fn flush(&mut self) -> bool {
        while let Some(client) = &mut self.client {
            if self.outgoing.is_empty() {
                return true;
            }
            match client.write(&self.outgoing) {
                Ok(0) => self.drop_client(),
                Ok(written) => {
                    self.outgoing.drain(..written);
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    return self.outgoing.len() < MAX_OUTGOING;
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => self.drop_client(),
            }
        }
        false
    }

    /// The ATEMP field: its kind, then ATEMP.
    fn atemp(&mut self, chip: &mut Mc68331) -> Vec<u8> {
        let (kind, atemp) = match Cpu32Target::halted(chip).ok().flatten() {
            None => {
                self.stale = None;
                (Kind::Running, 0)
            }
            Some(entry) => self
                .stale
                .map_or((Kind::Fresh, entry.atemp()), |atemp| (Kind::Stale, atemp)),
        };
        let mut field = vec![kind as u8];
        field.extend_from_slice(&atemp.to_le_bytes());
        field
    }

    /// The reply to `packet`.
    fn answer(&mut self, chip: &mut Mc68331, packet: &[u8]) -> Vec<u8> {
        let Some(request) = Request::parse(packet) else {
            return reply(Status::BadRequest, &[]);
        };
        let frozen = Cpu32Target::halted(chip).ok().flatten().is_some();
        let result = match request {
            // the raw exchange is not modeled yet
            Request::Transfer(_) => return reply(Status::BadRequest, &[]),
            Request::Hello => Ok(PROTOCOL_VERSION.to_le_bytes().to_vec()),
            Request::SetClock(hertz) => Ok(hertz.clamp(1, MAX_DSCLK_HZ).to_le_bytes().to_vec()),
            Request::Pins => {
                let freeze = if frozen { PIN_FREEZE } else { 0 };
                let mut body = vec![freeze | PIN_RESET | PIN_DS];
                body.extend_from_slice(&VDD_MILLIVOLTS.to_le_bytes());
                body.extend(self.atemp(chip));
                Ok(body)
            }
            Request::ResetHalt => {
                self.stale = None;
                chip.reset_halted().map(|_| self.atemp(chip))
            }
            Request::Halt => chip.halt().map(|_| self.atemp(chip)),
            _ if !frozen => return reply(Status::NoFreeze, &[]),
            Request::Go => chip.go().map(|()| {
                self.stale = None;
                Vec::new()
            }),
            Request::ReadRegister(register) => chip
                .read_register(register)
                .map(|value| value.to_le_bytes().to_vec()),
            Request::WriteRegister(register, value) => {
                chip.write_register(register, value).map(|()| Vec::new())
            }
            Request::ReadSystemRegister(register) => chip
                .read_system_register(register)
                .map(|value| value.to_le_bytes().to_vec()),
            Request::WriteSystemRegister(register, value) => chip
                .write_system_register(register, value)
                .map(|()| Vec::new()),
            Request::ReadMemory(address, size, length) => {
                let mut data = vec![0; length];
                function_code(chip, SystemRegister::Sfc)
                    .and_then(|space| chip.read_memory(address, space, size, &mut data))
                    .map(|()| data)
            }
            Request::WriteMemory(address, size, data) => function_code(chip, SystemRegister::Dfc)
                .and_then(|space| chip.write_memory(address, space, size, &data))
                .map(|()| Vec::new()),
            Request::Call(address) => chip.call(address).map(|()| {
                self.stale = None;
                Vec::new()
            }),
        };
        match result {
            Ok(body) => reply(Status::Ok, &body),
            Err(error) => reply(status(error), &[]),
        }
    }
}

/// The function code SFC or DFC holds.
fn function_code(
    chip: &mut Mc68331,
    register: SystemRegister,
) -> Result<FunctionCode, TargetError> {
    chip.read_system_register(register).map(FunctionCode::new)
}

#[cfg(test)]
mod tests {
    use super::Request;
    use super::Server;
    use super::Size;
    use super::code;
    use mc683xx_cpu32::cycles::Cycles;
    use mc683xx_mc68331::chip::Builder;
    use mc683xx_modules::chip_select::ChipSelect;
    use mc683xx_target::Cpu32Target;
    use mc683xx_target::address::Address;
    use mc683xx_target::register::Register;
    use mc683xx_target::register::SystemRegister;

    /// Where the boot ROM's program starts.
    const START: usize = 0x400;
    /// The ATEMP field's kind in a Pins reply.
    const PINS_KIND: usize = 4;
    /// The kind of an entry the server saw.
    const FRESH: u8 = 1;
    /// The kind of an entry it did not.
    const STALE: u8 = 2;

    #[test]
    fn an_entry_after_go_is_fresh_though_the_first_was_stale_and_kept() {
        let mut rom = vec![0; 0x1000];
        rom[4..8].copy_from_slice(&(START as u32).to_be_bytes());
        // BGND; BRA.B *
        rom[START..START + 4].copy_from_slice(&[0x4a, 0xfa, 0x60, 0xfe]);
        let mut builder = Builder::default();
        builder.memory(rom, false, &[ChipSelect::Boot]).unwrap();
        let mut chip = builder.build();
        chip.reset_halted().unwrap();
        chip.write_system_register(SystemRegister::Atemp, 7)
            .unwrap();
        let mut server = Server::bind("127.0.0.1:0", &mut chip).unwrap();
        let pins = server.answer(&mut chip, &[code::PINS]);
        assert_eq!(pins[PINS_KIND], STALE);
        assert_eq!(pins[PINS_KIND + 1..], 7u32.to_le_bytes());
        let atemp = SystemRegister::Atemp.code() as u8;
        let write = [code::WRITE_SYSTEM_REGISTER, atemp, 9, 0, 0, 0];
        assert_eq!(server.answer(&mut chip, &write), [0]);
        let pins = server.answer(&mut chip, &[code::PINS]);
        assert_eq!(pins[PINS_KIND + 1..], 7u32.to_le_bytes(), "kept");
        assert_eq!(server.answer(&mut chip, &[code::GO]), [0]);
        chip.run_for(Cycles(1000));
        assert!(Cpu32Target::halted(&mut chip).unwrap().is_some(), "BGND");
        assert_eq!(server.answer(&mut chip, &[code::PINS])[PINS_KIND], FRESH);
    }

    #[test]
    fn requests_parse_by_the_protocols_exact_lengths_and_ranges() {
        assert_eq!(Request::parse(&[0x0e]), Some(Request::Hello));
        assert_eq!(Request::parse(&[0x0e, 0]), None, "Hello takes no body");
        assert_eq!(Request::parse(&[]), None);
        assert_eq!(Request::parse(&[0x0f]), None, "no such code");
        assert_eq!(
            Request::parse(&[0x07, 15]),
            Some(Request::ReadRegister(Register::A7))
        );
        assert_eq!(Request::parse(&[0x07, 16]), None);
        assert_eq!(
            Request::parse(&[0x0a, 0x0e, 5, 0, 0, 0]),
            Some(Request::WriteSystemRegister(SystemRegister::Sfc, 5))
        );
        assert_eq!(Request::parse(&[0x09, 2]), None, "no select code 2");
        assert_eq!(
            Request::parse(&[0x0b, 0, 4, 0, 0, 1, 8]),
            Some(Request::ReadMemory(Address(0x400), Size::Word, 8))
        );
        assert_eq!(Request::parse(&[0x0b, 0, 4, 0, 0, 3, 8]), None, "size 3");
        assert_eq!(Request::parse(&[0x0b, 0, 4, 0, 0, 4, 8]), None, "size 4");
        assert_eq!(
            Request::parse(&[0x0b, 0, 4, 0, 0, 2, 6]),
            None,
            "6 bytes of longs"
        );
        assert_eq!(Request::parse(&[0x0b, 0, 4, 0, 0, 0, 57]), None, "57 bytes");
        assert_eq!(
            Request::parse(&[0x0c, 0, 4, 0, 0, 0, 0xaa]),
            Some(Request::WriteMemory(Address(0x400), Size::Byte, vec![0xaa]))
        );
        assert_eq!(Request::parse(&[0x0c, 0, 4, 0, 0, 0]), None, "no data");
        assert_eq!(
            Request::parse(&[0x01, 0x34, 0x12]),
            Some(Request::Transfer(vec![0x1234]))
        );
        assert_eq!(Request::parse(&[0x01, 0x34]), None, "half a word");
    }
}
