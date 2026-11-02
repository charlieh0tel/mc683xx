//! A session: gdb's remote serial protocol over a [`Target`].
//!
//! All-stop and single-threaded.  An emulated target runs only inside
//! the session, in quanta, and between quanta the stub looks for gdb's
//! Ctrl-C, so the core stays single-threaded and nothing runs while
//! gdb looks; its breakpoints, watchpoints and steps are hooks
//! ([`hooks`]).  A target under BDM control runs on its own once
//! resumed, and the stub looks in turn at it and at gdb; its
//! breakpoints are BGND planted in memory ([`planted`]) and its steps
//! its own.  Every hook installed and every breakpoint planted is
//! removed when the session ends, however it ends.

use std::fmt::Display;
use std::marker::PhantomData;
use std::rc::Rc;

use gdbstub::common::Signal;
use gdbstub::conn::Connection;
use gdbstub::conn::ConnectionExt;
use gdbstub::stub::DisconnectReason;
use gdbstub::stub::GdbStub;
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
use mc683xx_cpu32::cpu::hooks::HookId;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_target::address::Address;
use mc683xx_target::entry::Entry;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::register::SystemRegister;

use crate::Ending;
use crate::SessionError;
use crate::arch::Cpu32;
use crate::arch::GdbRegisters;
use crate::arch::Location;
use crate::protocol::EFAULT;
use crate::protocol::EINVAL;
use crate::protocol::EIO;
use crate::protocol::description;
use crate::protocol::failure;
use crate::target::AccessError;
use crate::target::Target;
use crate::target::TargetRun;
use hooks::Hits;
use hooks::Kind;
use hooks::Watchpoint;
use hooks::stop_reason;
use planted::BGND;
use planted::Breakpoint;

#[cfg(feature = "board")]
mod conditions;
mod hooks;
mod planted;

/// System clocks an emulated target runs between looks for gdb's
/// Ctrl-C: about 6 ms of a 16.78 MHz chip.
const QUANTUM: Cycles = Cycles(100_000);

/// Serves gdb on `connection` over `target`: [`crate::serve`].
pub(crate) fn serve<T, C>(
    target: &mut T,
    connection: C,
) -> Result<Ending, SessionError<T::Error, C::Error>>
where
    T: Target,
    T::Error: Display,
    C: ConnectionExt,
    C::Error: Display,
{
    target.halt().map_err(SessionError::Target)?;
    let emulated = target.with_cpu(|_| ()).is_some();
    let mut session = Session {
        target,
        emulated,
        function_code: FunctionCode::SUPERVISOR_DATA,
        hit: Rc::default(),
        hooked: Vec::new(),
        watchpoints: Vec::new(),
        planted: Vec::new(),
        action: Action::Continue,
        going: None,
    };
    let served = GdbStub::new(connection).run_blocking::<EventLoop<'_, T, C>>(&mut session);
    // a failed session's error is the one reported, not cleaning up's
    let cleaned = session.clean_up();
    let ending = match served {
        Ok(DisconnectReason::Kill) => Ending::Killed,
        Ok(_) => Ending::Detached,
        Err(error) => return Err(failure(error)),
    };
    cleaned.map_err(SessionError::Target)?;
    if ending == Ending::Detached {
        session.target.resume().map_err(SessionError::Target)?;
    }
    Ok(ending)
}

/// What gdb last asked the target to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Run until something stops it.
    Continue,
    /// One step.
    Step,
}

/// How gdb set the target going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Going {
    /// Resumed.
    Resumed,
    /// Stepped by the target from this RPC: a BGND it stops at is
    /// there, PCC being unspecified after a step.
    Stepped(u32),
}

/// A session: the target and what gdb set on it.
struct Session<'t, T: Target> {
    /// The target.
    target: &'t mut T,
    /// Whether it gives the CPU's controls: breakpoints, watchpoints
    /// and steps are then hooks.
    emulated: bool,
    /// The address space memory is read and written in.
    function_code: FunctionCode,
    /// The hit the stub's hooks recorded.
    hit: Hits,
    /// The hook breakpoints, by kind and gdb's address.
    hooked: Vec<(Kind, u32, HookId)>,
    /// The watchpoints.
    watchpoints: Vec<Watchpoint>,
    /// The software breakpoints planted, and any whose planting may
    /// have written BGND that could not be put back.
    planted: Vec<Breakpoint>,
    /// What gdb last asked.
    action: Action,
    /// How gdb last set the target going, while it may be running or
    /// its stop is not yet taken.
    going: Option<Going>,
}

impl<T: Target> Drop for Session<'_, T> {
    fn drop(&mut self) {
        self.remove_hooks();
    }
}

/// The value of an access, `None` if it failed for a reason other than
/// the target's own error.
fn reached<V, E>(access: Result<V, AccessError<E>>) -> Result<Option<V>, E> {
    match access {
        Ok(value) => Ok(Some(value)),
        Err(AccessError::Target(error)) => Err(error),
        Err(_) => Ok(None),
    }
}

impl<T: Target> Session<'_, T> {
    /// Runs the target a quantum, or looks at one running on its own,
    /// with the hit cleared: `Some` if it stopped for gdb.
    fn quantum(&mut self) -> Result<Option<SingleThreadStopReason<u32>>, T::Error> {
        self.hit.set(None);
        let hits = Rc::clone(&self.hit);
        let ran = self
            .target
            .run_for(QUANTUM, &move || hits.get().is_some())?;
        if let Some(hit) = self.hit.take() {
            self.going = None;
            return Ok(Some(stop_reason(hit)));
        }
        match ran {
            TargetRun::Quantum => Ok(None),
            other => self.stopped(other).map(Some),
        }
    }

    /// gdb's view of how the target stopped, the stop taken: for an
    /// entry into background mode, see [`entered`](Self::entered).
    fn stopped(&mut self, ran: TargetRun) -> Result<SingleThreadStopReason<u32>, T::Error> {
        let reason = match ran {
            TargetRun::Background(entry) => self.entered(entry)?,
            other => signaled(other),
        };
        self.going = None;
        Ok(reason)
    }

    /// gdb's view of why the target entered background mode for
    /// `entry`, at the BGND a step began on or else at PCC (CPU32RM
    /// 7.2.5.3).  A planted breakpoint's BGND has RPC put back on it.
    fn entered(&mut self, entry: Entry) -> Result<SingleThreadStopReason<u32>, T::Error> {
        Ok(match entry {
            Entry::Background => {
                let at = match self.going {
                    Some(Going::Stepped(at)) => Some(at),
                    _ => reached(self.target.read_system_register(SystemRegister::Pcc))?,
                };
                match at {
                    Some(at) if self.planted.iter().any(|planted| planted.address == at) => {
                        reached(self.target.write_system_register(SystemRegister::Rpc, at))?;
                        SingleThreadStopReason::SwBreak(())
                    }
                    _ => SingleThreadStopReason::Signal(Signal::SIGTRAP),
                }
            }
            Entry::DoubleBusFault { .. } => SingleThreadStopReason::Signal(Signal::SIGBUS),
            _ => SingleThreadStopReason::Signal(Signal::SIGTRAP),
        })
    }

    /// One step, and why it stopped: done, unless it ran into a
    /// breakpoint, a watchpoint, a BGND or a halt.  An emulated target
    /// steps by the stub's hook, another by its own means.
    fn step(&mut self) -> Result<SingleThreadStopReason<u32>, T::Error> {
        if self.emulated {
            self.going = None;
            return self.hook_step();
        }
        if let Some(at) = reached(self.target.read_system_register(SystemRegister::Rpc))? {
            self.going = Some(Going::Stepped(at));
        }
        match self.target.step()? {
            Some(
                TargetRun::Background(Entry::Breakpoint) | TargetRun::Quantum | TargetRun::Hook,
            )
            | None => {
                self.going = None;
                Ok(SingleThreadStopReason::DoneStep)
            }
            Some(other) => self.stopped(other),
        }
    }

    /// If the target may be running on its own, halts it and takes its
    /// stop as gdb's would be, so a breakpoint's RPC is put back; then
    /// removes every breakpoint planted.  Each is tried whatever the
    /// last did; the first of the target's errors.
    fn clean_up(&mut self) -> Result<(), T::Error> {
        let mut first = Ok(());
        if self.going.is_some() && !self.emulated {
            let ran = match self.target.run_for(Cycles(0), &|| false) {
                Ok(ran @ TargetRun::Background(_)) => Ok(ran),
                _ => self.target.halt(),
            };
            first = ran.and_then(|ran| self.stopped(ran)).map(|_| ());
        }
        self.going = None;
        let removed = reached(self.unplant_all()).map(|_| ());
        first.and(removed)
    }
}

/// The signal gdb is told for a stop that was none of the stub's
/// ([docs/gdb.md, Stops](https://github.com/charlieh0tel/mc683xx/blob/main/docs/gdb.md#stops)): the host's hook or a stop condition a
/// trap, a halt or a fault a bus error, a request an interrupt.
const fn signaled(ran: TargetRun) -> SingleThreadStopReason<u32> {
    SingleThreadStopReason::Signal(match ran {
        TargetRun::Quantum | TargetRun::Hook | TargetRun::Condition => Signal::SIGTRAP,
        TargetRun::Background(Entry::DoubleBusFault { .. })
        | TargetRun::Halted
        | TargetRun::Fault => Signal::SIGBUS,
        TargetRun::Background(_) => Signal::SIGTRAP,
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
        description(annex, offset, length, buf).map_err(TargetError::Errno)
    }
}

impl<T: Target> SingleThreadBase for Session<'_, T> {
    fn read_registers(&mut self, registers: &mut GdbRegisters) -> TargetResult<(), Self> {
        let target = &mut *self.target;
        *registers = GdbRegisters::read(|location| match location {
            Location::Register(register) => target.read_register(register),
            Location::System(register) => target.read_system_register(register),
        })
        .map_err(|_| TargetError::Errno(EIO))?;
        Ok(())
    }

    fn write_registers(&mut self, registers: &GdbRegisters) -> TargetResult<(), Self> {
        if registers.is_ragged() {
            return Err(TargetError::Errno(EINVAL));
        }
        let mut before = GdbRegisters::default();
        self.read_registers(&mut before)?;
        for (location, value) in registers.writes(&before) {
            match location {
                Location::Register(register) => self.target.write_register(register, value),
                Location::System(register) => self.target.write_system_register(register, value),
            }
            .map_err(|_| TargetError::Errno(EIO))?;
        }
        Ok(())
    }

    /// Memory as the program has it: a planted breakpoint's bytes read
    /// as the word it replaced.
    fn read_addrs(&mut self, address: u32, data: &mut [u8]) -> TargetResult<usize, Self> {
        let space = self.function_code;
        let read = self
            .target
            .read_memory(Address(address), space, data)
            .map_err(|_| TargetError::Errno(EFAULT))?;
        if read == 0 && !data.is_empty() {
            return Err(TargetError::Errno(EFAULT));
        }
        for (offset, index, byte) in self.overlaps(address, read, space) {
            data[offset] = self.planted[index].original[byte];
        }
        Ok(read)
    }

    /// A write over a planted breakpoint's bytes goes to the word it
    /// replaced, to be put back on its removal; BGND stays.
    fn write_addrs(&mut self, address: u32, data: &[u8]) -> TargetResult<(), Self> {
        let space = self.function_code;
        let overlaps = self.overlaps(address, data.len(), space);
        let mut written = data.to_vec();
        for &(offset, _, byte) in &overlaps {
            written[offset] = BGND[byte];
        }
        self.target
            .write_memory(Address(address), space, &written)
            .map_err(|_| TargetError::Errno(EFAULT))?;
        for (offset, index, byte) in overlaps {
            self.planted[index].original[byte] = data[offset];
        }
        Ok(())
    }

    fn support_resume(&mut self) -> Option<SingleThreadResumeOps<'_, Self>> {
        Some(self)
    }
}

impl<T: Target> SingleThreadResume for Session<'_, T> {
    /// A signal gdb passes is dropped: the CPU has none.
    fn resume(&mut self, _signal: Option<Signal>) -> Result<(), T::Error> {
        self.action = Action::Continue;
        self.going = Some(Going::Resumed);
        self.target.resume()
    }

    fn support_single_step(&mut self) -> Option<SingleThreadSingleStepOps<'_, Self>> {
        Some(self)
    }
}

impl<T: Target> SingleThreadSingleStep for Session<'_, T> {
    fn step(&mut self, _signal: Option<Signal>) -> Result<(), T::Error> {
        self.action = Action::Step;
        self.going = Some(Going::Resumed);
        Ok(())
    }
}

/// Hardware breakpoints and watchpoints are an emulated target's
/// alone.
impl<T: Target> Breakpoints for Session<'_, T> {
    fn support_sw_breakpoint(&mut self) -> Option<SwBreakpointOps<'_, Self>> {
        Some(self)
    }

    fn support_hw_breakpoint(&mut self) -> Option<HwBreakpointOps<'_, Self>> {
        self.emulated.then_some(self)
    }

    fn support_hw_watchpoint(&mut self) -> Option<HwWatchpointOps<'_, Self>> {
        self.emulated.then_some(self)
    }
}

impl<T: Target> SwBreakpoint for Session<'_, T> {
    fn add_sw_breakpoint(&mut self, address: u32, _kind: usize) -> TargetResult<bool, Self> {
        if self.emulated {
            return Ok(self.add_hook_breakpoint(Kind::Software, address));
        }
        self.plant(address).map_err(|_| TargetError::Errno(EFAULT))
    }

    /// Idempotent for a planted breakpoint, as the gdb manual asks of
    /// `z` packets.
    fn remove_sw_breakpoint(&mut self, address: u32, _kind: usize) -> TargetResult<bool, Self> {
        if self.emulated {
            return Ok(self.remove_hook_breakpoint(Kind::Software, address));
        }
        self.unplant(address)
            .map_err(|_| TargetError::Errno(EFAULT))
    }
}

impl<T: Target> HwBreakpoint for Session<'_, T> {
    fn add_hw_breakpoint(&mut self, address: u32, _kind: usize) -> TargetResult<bool, Self> {
        Ok(self.add_hook_breakpoint(Kind::Hardware, address))
    }

    fn remove_hw_breakpoint(&mut self, address: u32, _kind: usize) -> TargetResult<bool, Self> {
        Ok(self.remove_hook_breakpoint(Kind::Hardware, address))
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
    fn handle_monitor_cmd(
        &mut self,
        command: &[u8],
        mut out: ConsoleOutput<'_>,
    ) -> Result<(), T::Error> {
        let words: Vec<&str> = str::from_utf8(command)
            .map(|command| command.split_whitespace().collect())
            .unwrap_or_default();
        self.monitor(&words, &mut out)
    }
}

impl<T: Target> Session<'_, T> {
    /// gdb's `monitor` command, in words: `fc` shows the address space
    /// memory is read and written in and `fc N` sets it; `reset` resets
    /// the target; `stop` lists, arms and disarms stop conditions;
    /// `help` lists the commands.
    fn monitor(&mut self, words: &[&str], out: &mut ConsoleOutput<'_>) -> Result<(), T::Error> {
        match words {
            ["fc"] => outputln!(out, "fc {}", self.function_code.bits()),
            ["fc", code] => match code.parse::<u32>() {
                Ok(code) if code <= u32::from(FunctionCode::CPU_SPACE.bits()) => {
                    self.function_code = FunctionCode::new(code);
                    outputln!(out, "fc {code}");
                }
                _ => outputln!(out, "fc takes a function code, 0 to 7"),
            },
            ["reset"] if self.target.reset()? => {
                outputln!(
                    out,
                    "reset; `maintenance flush register-cache` to see the registers"
                );
            }
            ["reset"] => outputln!(out, "this target cannot reset"),
            #[cfg(feature = "board")]
            ["stop", words @ ..] => match self.target.conditions() {
                Some(conditions) => conditions::monitor(conditions, words, out),
                None => outputln!(out, "this target has no stop conditions"),
            },
            ["help"] => {
                outputln!(out, "fc [N]: show or set the function code memory goes to");
                outputln!(
                    out,
                    "reset: reset the target; over BDM, halted before its first instruction"
                );
                #[cfg(feature = "board")]
                conditions::help(out);
            }
            _ => outputln!(out, "unknown command; `monitor help` lists them"),
        }
        Ok(())
    }
}

/// The run loop for gdbstub: a quantum of the target's run, or a look
/// at one running on its own, then a look for gdb's bytes.
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

    /// Ctrl-C stops a running target: `SIGINT`, or the breakpoint or
    /// BGND a target under BDM control reached first.  One while the
    /// target is stopped already is nothing.
    fn on_interrupt(
        session: &mut Session<'t, T>,
    ) -> Result<Option<SingleThreadStopReason<u32>>, T::Error> {
        if session.going.is_none() {
            return Ok(None);
        }
        match session.target.halt()? {
            TargetRun::Background(Entry::Breakpoint) | TargetRun::Requested => {
                session.going = None;
                Ok(Some(SingleThreadStopReason::Signal(Signal::SIGINT)))
            }
            other => session.stopped(other).map(Some),
        }
    }
}
