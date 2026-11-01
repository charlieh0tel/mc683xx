//! `--bdm`: a virtual BDM cable.  The runner answers on a socket as the
//! m683xx BDM pod answers on USB (its host-pod protocol, version 2:
//! github.com/charlieh0tel/m683xx-pod, `docs/usb-protocol.md`), so a
//! debugger that drives a 68331 through the pod drives the board the
//! same way, through the chip's [`Cpu32Target`].
//!
//! One client at a time; another connection is closed at once.  The
//! server is polled between the runner's turns and, while the CPU is in
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
    /// Bytes read and not yet a whole message.
    pending: Vec<u8>,
    /// Whether the CPU was in background mode when the server started,
    /// an entry it did not see: its ATEMP is stale until it resumes.
    stale: bool,
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
            stale: Cpu32Target::halted(chip).ok().flatten().is_some(),
        })
    }

    /// Takes a new connection, closing any beyond the first, and
    /// answers every whole request the client has sent, not blocking.
    /// Whether it answered one.
    pub(crate) fn poll(&mut self, chip: &mut Mc68331) -> bool {
        while let Ok(Some(stream)) = self.listener.accept() {
            if self.client.is_none() {
                self.client = Some(stream);
                self.pending.clear();
            }
            // another is dropped, closing it
        }
        let mut answered = false;
        let mut buffer = [0; 256];
        loop {
            let Some(client) = &mut self.client else {
                return answered;
            };
            match client.read(&mut buffer) {
                Ok(0) => self.drop_client(),
                Ok(read) => self.pending.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => self.drop_client(),
            }
        }
        while let Some(&length) = self.pending.first() {
            let length = usize::from(length);
            if length == 0 || length > MAX_PACKET {
                // a broken stream
                self.drop_client();
                return answered;
            }
            if self.pending.len() <= length {
                break;
            }
            let packet: Vec<u8> = self.pending.drain(..=length).skip(1).collect();
            let answer = self.answer(chip, &packet);
            if !self.send(&answer) {
                return answered;
            }
            answered = true;
        }
        answered
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

    /// Closes the client's connection.
    fn drop_client(&mut self) {
        self.client = None;
        self.pending.clear();
    }

    /// Sends `packet` with its length byte; false, the client dropped,
    /// if it could not.
    fn send(&mut self, packet: &[u8]) -> bool {
        let Some(client) = &mut self.client else {
            return false;
        };
        let mut message = vec![packet.len() as u8];
        message.extend_from_slice(packet);
        let sent = client
            .set_nonblocking(false)
            .and_then(|()| client.write_all(&message))
            .and_then(|()| client.set_nonblocking(true));
        if sent.is_err() {
            self.drop_client();
            return false;
        }
        true
    }

    /// The ATEMP field: its kind, then ATEMP.
    fn atemp(&mut self, chip: &mut Mc68331) -> Vec<u8> {
        let (kind, atemp) = match Cpu32Target::halted(chip).ok().flatten() {
            None => {
                self.stale = false;
                (Kind::Running, 0)
            }
            Some(_) if self.stale => (Kind::Stale, chip.cpu().atemp()),
            Some(entry) => (Kind::Fresh, entry.atemp()),
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
                self.stale = false;
                chip.reset_halted().map(|_| self.atemp(chip))
            }
            Request::Halt => chip.halt().map(|_| self.atemp(chip)),
            _ if !frozen => return reply(Status::NoFreeze, &[]),
            Request::Go => chip.go().map(|()| Vec::new()),
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
            Request::Call(address) => call(chip, address).map(|()| Vec::new()),
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

/// CALL: RPC stacked at the active SP, in its data space, then
/// execution from `address`; a fault stacking leaves the CPU in
/// background mode with SP as it was (CPU32RM 7.2.8.13).
fn call(chip: &mut Mc68331, address: Address) -> Result<(), TargetError> {
    let supervisor = chip.read_system_register(SystemRegister::Sr)? & u32::from(SR_S) != 0;
    let space = if supervisor {
        FunctionCode::SUPERVISOR_DATA
    } else {
        FunctionCode::USER_DATA
    };
    let sp = chip.read_register(Register::A7)?.wrapping_sub(4);
    let rpc = chip.read_system_register(SystemRegister::Rpc)?;
    chip.write_memory(Address(sp), space, Size::Long, &rpc.to_be_bytes())?;
    chip.write_register(Register::A7, sp)?;
    chip.write_system_register(SystemRegister::Rpc, address.0)?;
    chip.go()
}

/// SR's S bit (CPU32RM 2.3.2).
const SR_S: u16 = 1 << 13;

#[cfg(test)]
mod tests {
    use super::Request;
    use super::Size;
    use mc683xx_target::address::Address;
    use mc683xx_target::register::Register;
    use mc683xx_target::register::SystemRegister;

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
