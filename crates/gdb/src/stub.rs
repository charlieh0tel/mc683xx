//! The stub: gdb's remote serial protocol on a connection, over a
//! [`Target`].
//!
//! All-stop and single-threaded: the target runs only inside
//! [`serve`], in quanta, and between quanta the stub looks for gdb's
//! Ctrl-C, so the core stays single-threaded and nothing runs while
//! gdb looks.
//!
//! Breakpoints, hardware or software, are code hooks: no instruction
//! is written to memory, so they work in ROM and firmware that sums
//! its code never sees them.  Watchpoints are read and write hooks.  A
//! single step is a code hook on every address, for one instruction.
//! Every hook the stub installs is removed when the session ends.

use std::cell::Cell;
use std::fmt::Display;
use std::io;
use std::io::Read;
use std::io::Write;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::TcpStream;
use std::net::ToSocketAddrs;
use std::ops::ControlFlow;
use std::ops::RangeInclusive;
use std::rc::Rc;
use std::thread;
use std::time::Duration;

use gdbstub::arch::Arch;
use gdbstub::common::Signal;
use gdbstub::conn::Connection;
use gdbstub::conn::ConnectionExt;
use gdbstub::stub::DisconnectReason;
use gdbstub::stub::GdbStub;
use gdbstub::stub::GdbStubError;
use gdbstub::stub::SingleThreadStopReason;
use gdbstub::stub::run_blocking::BlockingEventLoop;
use gdbstub::stub::run_blocking::Event;
use gdbstub::stub::run_blocking::WaitForStopReasonError;
use gdbstub::target;
use gdbstub::target::TargetError;
use gdbstub::target::TargetResult;
use gdbstub::target::ext::base::BaseOps;
use gdbstub::target::ext::base::singlethread::SingleThreadBase;
use gdbstub::target::ext::base::singlethread::SingleThreadResume;
use gdbstub::target::ext::base::singlethread::SingleThreadResumeOps;
use gdbstub::target::ext::base::singlethread::SingleThreadSingleStep;
use gdbstub::target::ext::base::singlethread::SingleThreadSingleStepOps;
use gdbstub::target::ext::breakpoints::Breakpoints;
use gdbstub::target::ext::breakpoints::BreakpointsOps;
use gdbstub::target::ext::breakpoints::HwBreakpoint;
use gdbstub::target::ext::breakpoints::HwBreakpointOps;
use gdbstub::target::ext::breakpoints::HwWatchpoint;
use gdbstub::target::ext::breakpoints::HwWatchpointOps;
use gdbstub::target::ext::breakpoints::SwBreakpoint;
use gdbstub::target::ext::breakpoints::SwBreakpointOps;
use gdbstub::target::ext::breakpoints::WatchKind;
use gdbstub::target::ext::monitor_cmd::ConsoleOutput;
use gdbstub::target::ext::monitor_cmd::MonitorCmd;
use gdbstub::target::ext::monitor_cmd::MonitorCmdOps;
use gdbstub::target::ext::monitor_cmd::outputln;
use gdbstub::target::ext::target_description_xml_override::TargetDescriptionXmlOverride;
use gdbstub::target::ext::target_description_xml_override::TargetDescriptionXmlOverrideOps;
use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::cpu::hooks::HookId;
use mc683xx_cpu32::cpu::hooks::MemoryAccess;
use mc683xx_cpu32::cycles::Cycles;

use crate::arch::Cpu32;
use crate::arch::GdbRegisters;
use crate::target::Target;
use crate::target::TargetRun;

/// System clocks the target runs between looks for gdb's Ctrl-C: about
/// 6 ms of a 16.78 MHz chip.
const QUANTUM: Cycles = Cycles(100_000);
/// Core runs a step makes at most: one that stops before the
/// instruction for the step's own hook, and one that runs it.
const STEP_RUNS: usize = 2;
/// Bytes before a watched address an access may start and still reach
/// it: a long's.
const ACCESS_REACH: u32 = 3;
/// gdb's error for memory it cannot read or write: EFAULT.
const EFAULT: u8 = 14;
/// gdb's error for a packet the stub refuses: EINVAL.
const EINVAL: u8 = 22;
/// The error a `qXfer` read answers for an annex there is none of
/// (the gdb manual, General Query Packets, `qXfer`).
const INVALID_ANNEX: u8 = 0;
/// The one annex of the target description.
const TARGET_XML: &[u8] = b"target.xml";

/// How a session ended.
///
/// More endings may come, so a match needs a wildcard arm:
///
/// ```compile_fail
/// use mc683xx_gdb::stub::Ending;
///
/// fn detached(ending: Ending) -> bool {
///     match ending {
///         Ending::Detached => true,
///         Ending::Killed => false,
///     }
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Ending {
    /// gdb detached: the target may run on without it.
    Detached,
    /// gdb killed the target: the host should stop running it.
    Killed,
}

/// Why a session ended other than as gdb asked.
///
/// More reasons may come, so a match needs a wildcard arm:
///
/// ```compile_fail
/// use mc683xx_gdb::stub::SessionError;
///
/// fn target<E, C>(error: SessionError<E, C>) -> Option<E> {
///     match error {
///         SessionError::Target(error) => Some(error),
///         SessionError::Connection(_) | SessionError::Protocol(_) => None,
///     }
/// }
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError<E, C> {
    /// The target's run failed.
    #[error("target: {0}")]
    Target(E),
    /// The connection failed, or gdb closed it.
    #[error("connection: {0}")]
    Connection(C),
    /// gdb said something the stub could not take.
    #[error("protocol: {0}")]
    Protocol(String),
}

/// The longest a [`Polled`] connection waits for gdb before it calls
/// its host's [`Waiter::idle`].
pub const POLL: Duration = Duration::from_millis(20);

/// A host that must stay responsive while the stub waits for gdb.
///
/// A Python host lets other threads run during each wait and runs its
/// signal handlers between them.
pub trait Waiter {
    /// What ends the session from the host's side.
    type Error;

    /// Runs `wait`, which blocks for at most [`POLL`].
    fn wait<R: Send>(&mut self, wait: impl FnOnce() -> R + Send) -> R;

    /// Called after each wait in which gdb sent nothing.
    ///
    /// # Errors
    ///
    /// The host's reason to end the session, which ends it as a
    /// [`PollError::Host`].
    fn idle(&mut self) -> Result<(), Self::Error>;
}

/// A [`Polled`] connection's error: the connection's, or the host's.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PollError<E> {
    /// The connection failed, or gdb closed it.
    #[error(transparent)]
    Connection(#[from] io::Error),
    /// The host's [`idle`](Waiter::idle) or `ready` ended the session,
    /// with its error, which this shows as its own: its message and
    /// its `source()`.
    #[error(transparent)]
    Host(E),
}

/// A TCP connection to gdb whose waits for bytes are [`POLL`]s, the
/// host's [`Waiter`] around and between them; for [`serve`].
#[derive(Debug)]
pub struct Polled<W> {
    /// The connection.
    stream: TcpStream,
    /// The host.
    waiter: W,
}

/// [`accept`], waiting for gdb in [`POLL`]s through `waiter`, and the
/// connection a [`Polled`] one.
///
/// # Errors
///
/// [`PollError::Connection`] if the listener cannot be bound or a
/// connection accepted or set up; [`PollError::Host`] if `ready` or
/// the waiter's [`idle`](Waiter::idle) fails.
pub fn accept_polled<W: Waiter>(
    address: impl ToSocketAddrs,
    ready: impl FnOnce(SocketAddr) -> Result<(), W::Error>,
    mut waiter: W,
) -> Result<Polled<W>, PollError<W::Error>> {
    let listener = TcpListener::bind(address)?;
    ready(listener.local_addr()?).map_err(PollError::Host)?;
    listener.set_nonblocking(true)?;
    let stream = loop {
        let accepted = waiter.wait(|| {
            let accepted = listener.accept();
            if accepted.as_ref().is_err_and(waiting) {
                thread::sleep(POLL);
            }
            accepted
        });
        match accepted {
            Ok((stream, _)) => break stream,
            Err(error) if waiting(&error) => waiter.idle().map_err(PollError::Host)?,
            Err(error) => return Err(error.into()),
        }
    };
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(POLL))?;
    Ok(Polled { stream, waiter })
}

/// Whether `error` is a wait that ended with nothing: a timeout, or a
/// signal.
fn waiting(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

impl<W: Waiter> Connection for Polled<W> {
    type Error = PollError<W::Error>;

    fn write(&mut self, byte: u8) -> Result<(), Self::Error> {
        Ok(Write::write_all(&mut self.stream, &[byte])?)
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        Ok(Write::write_all(&mut self.stream, bytes)?)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(Write::flush(&mut self.stream)?)
    }

    fn on_session_start(&mut self) -> Result<(), Self::Error> {
        Ok(self.stream.set_nodelay(true)?)
    }
}

impl<W: Waiter> ConnectionExt for Polled<W> {
    fn read(&mut self) -> Result<u8, Self::Error> {
        let stream = &self.stream;
        loop {
            let read = self.waiter.wait(|| {
                let mut byte = [0];
                (&*stream).read(&mut byte).map(|count| (count, byte[0]))
            });
            match read {
                Ok((0, _)) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into()),
                Ok((_, byte)) => return Ok(byte),
                Err(error) if waiting(&error) => self.waiter.idle().map_err(PollError::Host)?,
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Between a run's quanta: the host's wait around it lets other
    /// threads in (gdb's own client among them).
    fn peek(&mut self) -> Result<Option<u8>, Self::Error> {
        let stream = &self.stream;
        let peeked = self.waiter.wait(|| {
            stream.set_nonblocking(true)?;
            let mut byte = [0];
            let peeked = stream.peek(&mut byte);
            stream.set_nonblocking(false)?;
            peeked.map(|_| byte[0])
        });
        match peeked {
            Ok(byte) => Ok(Some(byte)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

/// Listens at `address` for one gdb, tells `ready` the address it
/// listens at (a port of 0 picks one), and takes the first connection.
///
/// # Errors
///
/// `ready`'s error, or the I/O error, as `E`, if the listener cannot
/// be bound or a connection accepted.
pub fn accept<E: From<io::Error>>(
    address: impl ToSocketAddrs,
    ready: impl FnOnce(SocketAddr) -> Result<(), E>,
) -> Result<TcpStream, E> {
    let listener = TcpListener::bind(address)?;
    ready(listener.local_addr()?)?;
    let (stream, _) = listener.accept()?;
    Ok(stream)
}

/// Serves gdb on `connection` until it detaches or kills the target.
///
/// The target runs only as gdb asks, so it stays where it is until
/// gdb continues or steps it.
///
/// # Errors
///
/// [`SessionError::Target`] if the target's run fails,
/// [`SessionError::Connection`] if the connection fails or gdb closes
/// it, and [`SessionError::Protocol`] if gdb sends what the stub cannot
/// take.
pub fn serve<T, C>(
    target: &mut T,
    connection: C,
) -> Result<Ending, SessionError<T::Error, C::Error>>
where
    T: Target,
    T::Error: Display,
    C: ConnectionExt,
    C::Error: Display,
{
    let mut session = Session {
        target,
        hit: Rc::default(),
        breakpoints: Vec::new(),
        watchpoints: Vec::new(),
        action: Action::Continue,
    };
    let served = GdbStub::new(connection).run_blocking::<EventLoop<'_, T, C>>(&mut session);
    match served {
        Ok(DisconnectReason::Kill) => Ok(Ending::Killed),
        Ok(_) => Ok(Ending::Detached),
        Err(error) => Err(failure(error)),
    }
}

/// gdbstub's error as ours.
fn failure<E: Display, C: Display>(error: GdbStubError<E, C>) -> SessionError<E, C> {
    let text = error.to_string();
    if error.is_connection_error() {
        return error
            .into_connection_error()
            .map_or(SessionError::Protocol(text), |(error, _)| {
                SessionError::Connection(error)
            });
    }
    error
        .into_target_error()
        .map_or(SessionError::Protocol(text), SessionError::Target)
}

/// What stopped the target for gdb: the first of the stub's hooks to
/// ask since the run began.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    /// A breakpoint, as gdb set it.
    Breakpoint(Kind),
    /// A watchpoint, of its kind, at the address gdb gave.
    Watch(WatchKind, u32),
    /// The step's hook.
    Step,
}

/// A breakpoint's kind, as gdb set it.  Both are code hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `Z0`: gdb's `break`.
    Software,
    /// `Z1`: gdb's `hbreak`.
    Hardware,
}

/// The stub's record of a hit, shared with its hooks.
type Hits = Rc<Cell<Option<Hit>>>;

/// Records `hit` unless one is recorded already.
fn record(hits: &Hits, hit: Hit) {
    if hits.get().is_none() {
        hits.set(Some(hit));
    }
}

/// What gdb last asked the target to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Run until something stops it.
    Continue,
    /// Run one instruction.
    Step,
}

/// A watchpoint gdb set.
#[derive(Debug)]
struct Watchpoint {
    /// Its address, as gdb gave it.
    address: u32,
    /// Its bytes.
    length: u32,
    /// What it watches.
    kind: WatchKind,
    /// Its read and write hooks.
    hooks: Vec<HookId>,
}

/// A session: the target and what gdb set on it.
struct Session<'t, T: Target> {
    /// The target.
    target: &'t mut T,
    /// The hit the stub's hooks recorded.
    hit: Hits,
    /// The breakpoints, by kind and gdb's address, with their hooks.
    breakpoints: Vec<(Kind, u32, HookId)>,
    /// The watchpoints.
    watchpoints: Vec<Watchpoint>,
    /// What gdb last asked.
    action: Action,
}

impl<T: Target> Drop for Session<'_, T> {
    fn drop(&mut self) {
        let breakpoints = self.breakpoints.drain(..).map(|(_, _, id)| id);
        let watchpoints = self.watchpoints.drain(..).flat_map(|watch| watch.hooks);
        let hooks: Vec<HookId> = breakpoints.chain(watchpoints).collect();
        self.target.with_cpu(|cpu| {
            for id in hooks {
                cpu.remove_hook(id);
            }
        });
    }
}

impl<T: Target> Session<'_, T> {
    /// Runs the target a quantum with the hit cleared: `Some` if it
    /// stopped for gdb.
    fn quantum(&mut self) -> Result<Option<SingleThreadStopReason<u32>>, T::Error> {
        self.hit.set(None);
        let hits = Rc::clone(&self.hit);
        let ran = self
            .target
            .run_for(QUANTUM, &move || hits.get().is_some())?;
        if let Some(hit) = self.hit.take() {
            return Ok(Some(stop_reason(hit)));
        }
        Ok(match ran {
            TargetRun::Quantum => None,
            other => Some(signaled(other)),
        })
    }

    /// One instruction: a code hook on every address stops the run
    /// before the next.  If the hook stopped it before the first (no
    /// hook had stopped there yet), the next run goes past it, as a
    /// run does past a hook's stop.  The CPU waiting in `stop` steps no
    /// further than a quantum's wait.
    fn step(&mut self) -> Result<SingleThreadStopReason<u32>, T::Error> {
        let hits = Rc::clone(&self.hit);
        let (id, pc, count) = self.target.with_cpu(|cpu| {
            let id = cpu.add_code_hook(everywhere(), move |_| {
                record(&hits, Hit::Step);
                ControlFlow::Break(())
            });
            (id, cpu.registers.pc, cpu.instructions())
        });
        let stepped = self.step_runs(pc, count);
        self.target.with_cpu(|cpu| cpu.remove_hook(id));
        stepped
    }

    /// `step`'s runs, from `pc` with `count` instructions run.
    fn step_runs(&mut self, pc: u32, count: u64) -> Result<SingleThreadStopReason<u32>, T::Error> {
        let mut ran = TargetRun::Quantum;
        let mut watched = None;
        for _ in 0..STEP_RUNS {
            self.hit.set(None);
            let hits = Rc::clone(&self.hit);
            ran = self
                .target
                .run_for(QUANTUM, &move || hits.get().is_some())?;
            if let Some(hit @ Hit::Watch(..)) = self.hit.get() {
                watched = Some(hit);
            }
            let moved = self
                .target
                .with_cpu(|cpu| cpu.instructions() != count || cpu.registers.pc != pc);
            if moved || ran != TargetRun::Hook {
                break;
            }
        }
        Ok(match (watched, ran) {
            (Some(hit), _) => stop_reason(hit),
            (None, TargetRun::Quantum | TargetRun::Hook) => SingleThreadStopReason::DoneStep,
            (None, other) => signaled(other),
        })
    }

    /// Adds a breakpoint of `kind` at `address`: a code hook there.
    fn add_breakpoint(&mut self, kind: Kind, address: u32) -> bool {
        if self
            .breakpoints
            .iter()
            .any(|&(other, at, _)| (other, at) == (kind, address))
        {
            return true;
        }
        let hits = Rc::clone(&self.hit);
        let id = self.target.with_cpu(|cpu| {
            let at = cpu.memory.mask(Address(address));
            cpu.add_code_hook(at..=at, move |_| {
                record(&hits, Hit::Breakpoint(kind));
                ControlFlow::Break(())
            })
        });
        self.breakpoints.push((kind, address, id));
        true
    }

    /// Removes the breakpoint of `kind` at `address`; whether there was
    /// one.
    fn remove_breakpoint(&mut self, kind: Kind, address: u32) -> bool {
        let Some(index) = self
            .breakpoints
            .iter()
            .position(|&(other, at, _)| (other, at) == (kind, address))
        else {
            return false;
        };
        let (_, _, id) = self.breakpoints.remove(index);
        self.target.with_cpu(|cpu| cpu.remove_hook(id))
    }

    /// Adds a watchpoint: read hooks, write hooks or both on the bytes,
    /// each asking to stop for an access that overlaps them.  The stop
    /// comes once the instruction ends.
    fn add_watchpoint(&mut self, address: u32, length: u32, kind: WatchKind) -> bool {
        let Some(last_offset) = length.checked_sub(1) else {
            return false;
        };
        let hits = &self.hit;
        let hooks = self.target.with_cpu(|cpu| {
            let first = cpu.memory.mask(Address(address)).0;
            let last = first.saturating_add(last_offset);
            let reach = Address(first.saturating_sub(ACCESS_REACH))..=Address(last);
            let hook = |hits: Hits| {
                move |access: &MemoryAccess| {
                    let start = access.address.0;
                    let end = start.saturating_add(access.size.bytes() - 1);
                    if start <= last && end >= first {
                        record(&hits, Hit::Watch(kind, address));
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                }
            };
            let mut hooks = Vec::new();
            if matches!(kind, WatchKind::Read | WatchKind::ReadWrite) {
                hooks.push(cpu.add_read_hook(reach.clone(), hook(Rc::clone(hits))));
            }
            if matches!(kind, WatchKind::Write | WatchKind::ReadWrite) {
                hooks.push(cpu.add_write_hook(reach, hook(Rc::clone(hits))));
            }
            hooks
        });
        self.watchpoints.push(Watchpoint {
            address,
            length,
            kind,
            hooks,
        });
        true
    }

    /// Removes a watchpoint; whether there was one.
    fn remove_watchpoint(&mut self, address: u32, length: u32, kind: WatchKind) -> bool {
        let Some(index) = self
            .watchpoints
            .iter()
            .position(|watch| (watch.address, watch.length, watch.kind) == (address, length, kind))
        else {
            return false;
        };
        let watch = self.watchpoints.remove(index);
        self.target.with_cpu(|cpu| {
            for id in watch.hooks {
                cpu.remove_hook(id);
            }
        });
        true
    }
}

/// Every address, for the step's hook.
const fn everywhere() -> RangeInclusive<Address> {
    Address(0)..=Address(u32::MAX)
}

/// What gdb is told of `hit`.
const fn stop_reason(hit: Hit) -> SingleThreadStopReason<u32> {
    match hit {
        Hit::Breakpoint(Kind::Software) => SingleThreadStopReason::SwBreak(()),
        Hit::Breakpoint(Kind::Hardware) => SingleThreadStopReason::HwBreak(()),
        Hit::Watch(kind, addr) => SingleThreadStopReason::Watch {
            tid: (),
            kind,
            addr,
        },
        Hit::Step => SingleThreadStopReason::DoneStep,
    }
}

/// The signal gdb is told for a stop that was none of the stub's
/// ([docs/gdb.md, Stops](https://github.com/charlieh0tel/mc683xx/blob/main/docs/gdb.md#stops)): the host's hook a trap, a halt or a fault a
/// bus error, a request an interrupt.
const fn signaled(ran: TargetRun) -> SingleThreadStopReason<u32> {
    SingleThreadStopReason::Signal(match ran {
        TargetRun::Quantum | TargetRun::Hook => Signal::SIGTRAP,
        TargetRun::Halted | TargetRun::Fault => Signal::SIGBUS,
        TargetRun::Requested => Signal::SIGINT,
    })
}

impl<T: Target> target::Target for Session<'_, T> {
    type Arch = Cpu32;
    type Error = T::Error;

    fn base_ops(&mut self) -> BaseOps<'_, Cpu32, T::Error> {
        BaseOps::SingleThread(self)
    }

    fn support_breakpoints(&mut self) -> Option<BreakpointsOps<'_, Self>> {
        Some(self)
    }

    fn support_monitor_cmd(&mut self) -> Option<MonitorCmdOps<'_, Self>> {
        Some(self)
    }

    fn support_target_description_xml_override(
        &mut self,
    ) -> Option<TargetDescriptionXmlOverrideOps<'_, Self>> {
        Some(self)
    }
}

/// The architecture's description, served here rather than by gdbstub,
/// which ends the session on a read of any other annex: that is an
/// error reply instead.
impl<T: Target> TargetDescriptionXmlOverride for Session<'_, T> {
    fn target_description_xml(
        &self,
        annex: &[u8],
        offset: u64,
        length: usize,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        if annex != TARGET_XML {
            return Err(TargetError::Errno(INVALID_ANNEX));
        }
        let xml = Cpu32::target_description_xml()
            .unwrap_or_default()
            .trim()
            .as_bytes();
        let start = usize::try_from(offset).map_or(xml.len(), |offset| offset.min(xml.len()));
        let end = start.saturating_add(length.min(buf.len())).min(xml.len());
        let data = &xml[start..end];
        buf[..data.len()].copy_from_slice(data);
        Ok(data.len())
    }
}

impl<T: Target> SingleThreadBase for Session<'_, T> {
    fn read_registers(&mut self, registers: &mut GdbRegisters) -> TargetResult<(), Self> {
        *registers = self.target.with_cpu(|cpu| GdbRegisters::of(&cpu.registers));
        Ok(())
    }

    fn write_registers(&mut self, registers: &GdbRegisters) -> TargetResult<(), Self> {
        if registers.is_ragged() {
            return Err(TargetError::Errno(EINVAL));
        }
        self.target
            .with_cpu(|cpu| registers.store(cpu.registers_mut()));
        Ok(())
    }

    fn read_addrs(&mut self, address: u32, data: &mut [u8]) -> TargetResult<usize, Self> {
        match self.target.peek(Address(address), data) {
            0 if !data.is_empty() => Err(TargetError::Errno(EFAULT)),
            read => Ok(read),
        }
    }

    fn write_addrs(&mut self, address: u32, data: &[u8]) -> TargetResult<(), Self> {
        self.target
            .poke(Address(address), data)
            .map_err(|_| TargetError::Errno(EFAULT))
    }

    fn support_resume(&mut self) -> Option<SingleThreadResumeOps<'_, Self>> {
        Some(self)
    }
}

impl<T: Target> SingleThreadResume for Session<'_, T> {
    /// A signal gdb passes is dropped: the CPU has none.
    fn resume(&mut self, _signal: Option<Signal>) -> Result<(), T::Error> {
        self.action = Action::Continue;
        Ok(())
    }

    fn support_single_step(&mut self) -> Option<SingleThreadSingleStepOps<'_, Self>> {
        Some(self)
    }
}

impl<T: Target> SingleThreadSingleStep for Session<'_, T> {
    fn step(&mut self, _signal: Option<Signal>) -> Result<(), T::Error> {
        self.action = Action::Step;
        Ok(())
    }
}

impl<T: Target> Breakpoints for Session<'_, T> {
    fn support_sw_breakpoint(&mut self) -> Option<SwBreakpointOps<'_, Self>> {
        Some(self)
    }

    fn support_hw_breakpoint(&mut self) -> Option<HwBreakpointOps<'_, Self>> {
        Some(self)
    }

    fn support_hw_watchpoint(&mut self) -> Option<HwWatchpointOps<'_, Self>> {
        Some(self)
    }
}

impl<T: Target> SwBreakpoint for Session<'_, T> {
    fn add_sw_breakpoint(&mut self, address: u32, _kind: usize) -> TargetResult<bool, Self> {
        Ok(self.add_breakpoint(Kind::Software, address))
    }

    fn remove_sw_breakpoint(&mut self, address: u32, _kind: usize) -> TargetResult<bool, Self> {
        Ok(self.remove_breakpoint(Kind::Software, address))
    }
}

impl<T: Target> HwBreakpoint for Session<'_, T> {
    fn add_hw_breakpoint(&mut self, address: u32, _kind: usize) -> TargetResult<bool, Self> {
        Ok(self.add_breakpoint(Kind::Hardware, address))
    }

    fn remove_hw_breakpoint(&mut self, address: u32, _kind: usize) -> TargetResult<bool, Self> {
        Ok(self.remove_breakpoint(Kind::Hardware, address))
    }
}

impl<T: Target> HwWatchpoint for Session<'_, T> {
    fn add_hw_watchpoint(
        &mut self,
        address: u32,
        length: u32,
        kind: WatchKind,
    ) -> TargetResult<bool, Self> {
        Ok(self.add_watchpoint(address, length, kind))
    }

    fn remove_hw_watchpoint(
        &mut self,
        address: u32,
        length: u32,
        kind: WatchKind,
    ) -> TargetResult<bool, Self> {
        Ok(self.remove_watchpoint(address, length, kind))
    }
}

impl<T: Target> MonitorCmd for Session<'_, T> {
    /// `monitor reset` resets the target; `monitor help` lists the
    /// commands.
    fn handle_monitor_cmd(
        &mut self,
        command: &[u8],
        mut out: ConsoleOutput<'_>,
    ) -> Result<(), T::Error> {
        match command {
            b"reset" if self.target.reset() => {
                outputln!(out, "reset; `flushregs` to see the registers");
            }
            b"reset" => outputln!(out, "this target cannot reset"),
            b"help" => outputln!(out, "reset: reset the target"),
            _ => outputln!(out, "unknown command; `monitor help` lists them"),
        }
        Ok(())
    }
}

/// The run loop for gdbstub: quanta of the target's run, a look for
/// gdb's bytes between them.
struct EventLoop<'t, T, C>(PhantomData<(&'t mut T, C)>);

impl<'t, T, C> BlockingEventLoop for EventLoop<'t, T, C>
where
    T: Target,
    C: ConnectionExt,
{
    type Target = Session<'t, T>;
    type Connection = C;
    type StopReason = SingleThreadStopReason<u32>;

    fn wait_for_stop_reason(
        session: &mut Session<'t, T>,
        connection: &mut C,
    ) -> Result<
        Event<SingleThreadStopReason<u32>>,
        WaitForStopReasonError<T::Error, <C as Connection>::Error>,
    > {
        if session.action == Action::Step {
            return session
                .step()
                .map(Event::TargetStopped)
                .map_err(WaitForStopReasonError::Target);
        }
        loop {
            if let Some(reason) = session.quantum().map_err(WaitForStopReasonError::Target)? {
                return Ok(Event::TargetStopped(reason));
            }
            if connection
                .peek()
                .map_err(WaitForStopReasonError::Connection)?
                .is_some()
            {
                let byte = connection
                    .read()
                    .map_err(WaitForStopReasonError::Connection)?;
                return Ok(Event::IncomingData(byte));
            }
        }
    }

    fn on_interrupt(
        _session: &mut Session<'t, T>,
    ) -> Result<Option<SingleThreadStopReason<u32>>, T::Error> {
        Ok(Some(SingleThreadStopReason::Signal(Signal::SIGINT)))
    }
}
