//! The stub over a CPU32 under BDM control: gdb's remote serial
//! protocol on a connection, over a [`Cpu32Target`], a BDM pod on
//! silicon or the emulated chip.
//!
//! All-stop.  The target runs on its own between a resume and its next
//! entry into background mode; meanwhile the stub looks in turn at
//! [`Cpu32Target::halted`] and at gdb's connection, for its Ctrl-C.  A
//! single step is [`Cpu32Target::step`], which may stop past more than
//! one instruction or at a handler; gdb reads where from PC.
//!
//! A software breakpoint is BGND written over the instruction (CPU32RM
//! 4.4, BGND), so it needs memory the target can write: one in ROM is
//! refused.  BGND enters background mode with PCC at the breakpoint and
//! RPC past it; the stub puts RPC back on the breakpoint, so the
//! instruction runs once gdb has removed it.  Each breakpoint is
//! removed from the address space it was planted in, and only if BGND
//! is still there.  When the session ends, however it ends, a running
//! target is halted and every breakpoint still planted removed.  Memory
//! is read and written in one address space, supervisor data unless
//! `monitor fc` names another.
//!
//! The target must run on its own once resumed: silicon does, and an
//! emulated chip does only if its host runs it, in its
//! [`Cpu32Target::halted`] for one.  gdbstub answers `?` itself, with
//! `SIGTRAP`, whatever stopped the target before gdb attached.

use std::fmt::Display;
use std::marker::PhantomData;
use std::thread;
use std::time::Duration;

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
use gdbstub::target::ext::breakpoints::SwBreakpoint;
use gdbstub::target::ext::breakpoints::SwBreakpointOps;
use gdbstub::target::ext::monitor_cmd::ConsoleOutput;
use gdbstub::target::ext::monitor_cmd::MonitorCmd;
use gdbstub::target::ext::monitor_cmd::MonitorCmdOps;
use gdbstub::target::ext::monitor_cmd::outputln;
use gdbstub::target::ext::target_description_xml_override::TargetDescriptionXmlOverride;
use gdbstub::target::ext::target_description_xml_override::TargetDescriptionXmlOverrideOps;
use mc683xx_target::Cpu32Target;
use mc683xx_target::address::Address;
use mc683xx_target::entry::Entry;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::register::SystemRegister;
use mc683xx_target::size::Size;

use crate::arch::Cpu32;
use crate::arch::GdbRegisters;
use crate::arch::Location;
use crate::protocol::EFAULT;
use crate::protocol::EINVAL;
use crate::protocol::EIO;
use crate::protocol::description;
use crate::protocol::failure;
use crate::stub::Ending;
use crate::stub::SessionError;

/// BGND (CPU32RM 4.4, BGND): a software breakpoint.
const BGND: [u8; 2] = [0x4a, 0xfa];
/// How long the stub sleeps between looks at a running target and at
/// gdb.
const POLL_TICK: Duration = Duration::from_millis(1);

/// Serves gdb on `connection` over `target` until gdb detaches, which
/// resumes the target, or kills it, which leaves it halted.  The target
/// is halted first, so gdb finds it stopped.
///
/// # Errors
///
/// [`SessionError::Target`] if the target cannot be halted, a call on
/// it fails, or putting it back as it was when the session ends fails
/// (a session that failed otherwise reports that failure, not this), [`SessionError::Connection`] if the connection fails
/// or gdb closes it, and [`SessionError::Protocol`] if gdb sends what
/// the stub cannot take.
pub fn serve<T, C>(
    target: &mut T,
    connection: C,
) -> Result<Ending, SessionError<T::Error, C::Error>>
where
    T: Cpu32Target,
    T::Error: Display,
    C: ConnectionExt,
    C::Error: Display,
{
    target.halt().map_err(SessionError::Target)?;
    let mut session = Session {
        target,
        function_code: FunctionCode::SUPERVISOR_DATA,
        breakpoints: Vec::new(),
        action: Action::Continue,
        pending: None,
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
        session.target.go().map_err(SessionError::Target)?;
    }
    Ok(ending)
}

/// What gdb last asked the target to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Run until it stops.
    Continue,
    /// One step.
    Step,
}

/// A software breakpoint the session planted, or may have.
#[derive(Debug, Clone, Copy)]
struct Breakpoint {
    /// Its address.
    address: u32,
    /// The address space it was planted in.
    function_code: FunctionCode,
    /// The word BGND replaced.
    original: [u8; 2],
}

/// A session: the target and what the stub has done to it.
struct Session<'t, T: Cpu32Target> {
    /// The target.
    target: &'t mut T,
    /// The address space memory is read and written in.
    function_code: FunctionCode,
    /// The software breakpoints planted, and any whose planting may
    /// have written BGND that could not be put back.
    breakpoints: Vec<Breakpoint>,
    /// What gdb last asked.
    action: Action,
    /// How the target was last set going, while it may be running or
    /// its stop is not yet taken.
    pending: Option<Going>,
}

/// How the target was set going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Going {
    /// Resumed.
    Resumed,
    /// Stepped from this RPC: a BGND it stops at is there, PCC being
    /// unspecified after a step.
    Stepped(u32),
}

impl<T: Cpu32Target> Session<'_, T> {
    /// gdb's view of why the target entered background mode for
    /// `entry`, at the BGND a step began on or else at PCC (CPU32RM
    /// 7.2.5.3).  A planted breakpoint's BGND has RPC put back on it.
    /// The stop is taken only once this succeeds.
    fn stopped(&mut self, entry: Entry) -> Result<SingleThreadStopReason<u32>, T::Error> {
        let reason = match entry {
            Entry::Background => {
                let at = match self.pending {
                    Some(Going::Stepped(at)) => at,
                    _ => self.target.read_system_register(SystemRegister::Pcc)?,
                };
                if self.breakpoints.iter().any(|planted| planted.address == at) {
                    self.target.write_system_register(SystemRegister::Rpc, at)?;
                    SingleThreadStopReason::SwBreak(())
                } else {
                    SingleThreadStopReason::Signal(Signal::SIGTRAP)
                }
            }
            Entry::DoubleBusFault { .. } => SingleThreadStopReason::Signal(Signal::SIGBUS),
            _ => SingleThreadStopReason::Signal(Signal::SIGTRAP),
        };
        self.pending = None;
        Ok(reason)
    }

    /// One step, and why it stopped: done, unless it ran into a BGND
    /// or a double bus fault.
    fn step(&mut self) -> Result<SingleThreadStopReason<u32>, T::Error> {
        let at = self.target.read_system_register(SystemRegister::Rpc)?;
        self.pending = Some(Going::Stepped(at));
        match self.target.step()? {
            Entry::Breakpoint => {
                self.pending = None;
                Ok(SingleThreadStopReason::DoneStep)
            }
            other => self.stopped(other),
        }
    }

    /// If the target may be running, halts it and takes its stop as
    /// gdb's would be, so a breakpoint's RPC is put back; then removes
    /// every breakpoint.  Each is tried whatever the last did; the
    /// first error.
    fn clean_up(&mut self) -> Result<(), T::Error> {
        let mut first = Ok(());
        if self.pending.is_some() {
            let entry = match self.target.halted() {
                Ok(Some(entry)) => Ok(entry),
                _ => self.target.halt(),
            };
            first = entry.and_then(|entry| self.stopped(entry)).map(|_| ());
        }
        let removed = self.remove_breakpoints();
        first.and(removed)
    }

    /// Plants BGND at `address`, an even one, in the session's address
    /// space: false if the word there cannot be read, or does not read
    /// back as BGND, as in ROM.  A breakpoint whose BGND was written
    /// over since is planted again.  If BGND was written and the word
    /// cannot be put back, the breakpoint is kept for the session's
    /// end to try again.
    fn add_breakpoint(&mut self, address: u32) -> Result<bool, T::Error> {
        let space = self.function_code;
        if let Some(index) = self.breakpoint(address, space) {
            if self.word(address, space)? == BGND {
                return Ok(true);
            }
            self.breakpoints.remove(index);
        }
        if !address.is_multiple_of(2) {
            return Ok(false);
        }
        let Ok(original) = self.word(address, space) else {
            return Ok(false);
        };
        let planted = Breakpoint {
            address,
            function_code: space,
            original,
        };
        self.breakpoints.push(planted);
        let written = self
            .target
            .write_memory(Address(address), space, Size::Word, &BGND)
            .and_then(|()| self.word(address, space));
        if written.is_ok_and(|word| word == BGND) {
            return Ok(true);
        }
        if self.put_back(planted).unwrap_or(false) {
            self.breakpoints.pop();
        }
        Ok(false)
    }

    /// Puts back the words breakpoints at `address` replaced, in each
    /// one's address space (gdb names none): false if one did not read
    /// back, which is kept for the session's end to try again.
    /// Removing one there is none of is done already.  On an error the
    /// rest are kept.
    fn remove_breakpoint(&mut self, address: u32) -> Result<bool, T::Error> {
        let mut all = true;
        let mut index = 0;
        while index < self.breakpoints.len() {
            let planted = self.breakpoints[index];
            if planted.address == address {
                if self.put_back(planted)? {
                    self.breakpoints.remove(index);
                    continue;
                }
                all = false;
            }
            index += 1;
        }
        Ok(all)
    }

    /// Puts back the word `planted` replaced if BGND is still there (a
    /// word the program or gdb wrote over it since stands): whether the
    /// word there is no longer BGND.
    fn put_back(&mut self, planted: Breakpoint) -> Result<bool, T::Error> {
        let space = planted.function_code;
        if self.word(planted.address, space)? == BGND {
            self.target.write_memory(
                Address(planted.address),
                space,
                Size::Word,
                &planted.original,
            )?;
            return Ok(self.word(planted.address, space)? == planted.original);
        }
        Ok(true)
    }

    /// Removes every breakpoint; the first error, after trying them all.
    fn remove_breakpoints(&mut self) -> Result<(), T::Error> {
        let mut addresses: Vec<u32> = self
            .breakpoints
            .iter()
            .map(|planted| planted.address)
            .collect();
        addresses.sort_unstable();
        addresses.dedup();
        let mut first = Ok(());
        for address in addresses {
            let removed = self.remove_breakpoint(address).map(|_| ());
            if first.is_ok() {
                first = removed;
            }
        }
        first
    }

    /// The bytes of breakpoints planted in `space` that fall in `length`
    /// bytes at `address`: each byte's offset there, its breakpoint's
    /// index and the byte's index in the word.
    fn overlaps(
        &self,
        address: u32,
        length: usize,
        space: FunctionCode,
    ) -> Vec<(usize, usize, usize)> {
        let mut bytes = Vec::new();
        for (index, planted) in self.breakpoints.iter().enumerate() {
            if planted.function_code != space {
                continue;
            }
            for byte in 0..BGND.len() {
                let offset = planted
                    .address
                    .wrapping_add(byte as u32)
                    .wrapping_sub(address);
                if (offset as usize) < length {
                    bytes.push((offset as usize, index, byte));
                }
            }
        }
        bytes
    }

    /// The index of the breakpoint at `address` in `space`, if there is
    /// one.
    fn breakpoint(&self, address: u32, space: FunctionCode) -> Option<usize> {
        self.breakpoints
            .iter()
            .position(|planted| planted.address == address && planted.function_code == space)
    }

    /// The word at `address` in `space`.
    fn word(&mut self, address: u32, space: FunctionCode) -> Result<[u8; 2], T::Error> {
        let mut word = [0; 2];
        self.target
            .read_memory(Address(address), space, Size::Word, &mut word)?;
        Ok(word)
    }

    /// The size to move `length` bytes at `address` in: words if both
    /// are even, else bytes.
    fn size(address: u32, length: usize) -> Size {
        if address.is_multiple_of(2) && length.is_multiple_of(2) {
            Size::Word
        } else {
            Size::Byte
        }
    }
}

impl<T: Cpu32Target> target::Target for Session<'_, T> {
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

impl<T: Cpu32Target> TargetDescriptionXmlOverride for Session<'_, T> {
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

impl<T: Cpu32Target> SingleThreadBase for Session<'_, T> {
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
        self.target
            .read_memory(
                Address(address),
                space,
                Self::size(address, data.len()),
                data,
            )
            .map_err(|_| TargetError::Errno(EFAULT))?;
        for (offset, index, byte) in self.overlaps(address, data.len(), space) {
            data[offset] = self.breakpoints[index].original[byte];
        }
        Ok(data.len())
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
            .write_memory(
                Address(address),
                space,
                Self::size(address, data.len()),
                &written,
            )
            .map_err(|_| TargetError::Errno(EFAULT))?;
        for (offset, index, byte) in overlaps {
            self.breakpoints[index].original[byte] = data[offset];
        }
        Ok(())
    }

    fn support_resume(&mut self) -> Option<SingleThreadResumeOps<'_, Self>> {
        Some(self)
    }
}

impl<T: Cpu32Target> SingleThreadResume for Session<'_, T> {
    /// A signal gdb passes is dropped: the CPU has none.
    fn resume(&mut self, _signal: Option<Signal>) -> Result<(), T::Error> {
        self.action = Action::Continue;
        self.pending = Some(Going::Resumed);
        self.target.go()
    }

    fn support_single_step(&mut self) -> Option<SingleThreadSingleStepOps<'_, Self>> {
        Some(self)
    }
}

impl<T: Cpu32Target> SingleThreadSingleStep for Session<'_, T> {
    fn step(&mut self, _signal: Option<Signal>) -> Result<(), T::Error> {
        self.action = Action::Step;
        Ok(())
    }
}

impl<T: Cpu32Target> Breakpoints for Session<'_, T> {
    fn support_sw_breakpoint(&mut self) -> Option<SwBreakpointOps<'_, Self>> {
        Some(self)
    }
}

impl<T: Cpu32Target> SwBreakpoint for Session<'_, T> {
    fn add_sw_breakpoint(&mut self, address: u32, _kind: usize) -> TargetResult<bool, Self> {
        self.add_breakpoint(address)
            .map_err(|_| TargetError::Errno(EFAULT))
    }

    /// Idempotent, as the gdb manual asks of `z` packets.
    fn remove_sw_breakpoint(&mut self, address: u32, _kind: usize) -> TargetResult<bool, Self> {
        self.remove_breakpoint(address)
            .map_err(|_| TargetError::Errno(EFAULT))
    }
}

impl<T: Cpu32Target> MonitorCmd for Session<'_, T> {
    /// `monitor fc` shows the address space memory is read and written
    /// in and `monitor fc N` sets it; `monitor reset` resets the target
    /// halted; `monitor help` lists the commands.
    fn handle_monitor_cmd(
        &mut self,
        command: &[u8],
        mut out: ConsoleOutput<'_>,
    ) -> Result<(), T::Error> {
        let command = String::from_utf8_lossy(command);
        let words: Vec<&str> = command.split_whitespace().collect();
        match words.as_slice() {
            ["fc"] => outputln!(out, "fc {}", self.function_code.bits()),
            ["fc", code] => match code.parse::<u32>() {
                Ok(code) if code <= u32::from(FunctionCode::CPU_SPACE.bits()) => {
                    self.function_code = FunctionCode::new(code);
                    outputln!(out, "fc {code}");
                }
                _ => outputln!(out, "fc takes a function code, 0 to 7"),
            },
            ["reset"] => {
                let entry = self.target.reset_halted()?;
                outputln!(
                    out,
                    "reset, halted: {entry}; `maintenance flush register-cache` to see the registers"
                );
            }
            ["help"] => {
                outputln!(out, "fc [N]: show or set the function code memory goes to");
                outputln!(
                    out,
                    "reset: reset the target, halted before its first instruction"
                );
            }
            _ => outputln!(out, "unknown command; `monitor help` lists them"),
        }
        Ok(())
    }
}

/// The run loop for gdbstub: a look at the target, a look for gdb's
/// bytes, a short sleep.
struct EventLoop<'t, T, C>(PhantomData<(&'t mut T, C)>);

impl<'t, T, C> BlockingEventLoop for EventLoop<'t, T, C>
where
    T: Cpu32Target,
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
            if let Some(entry) = session
                .target
                .halted()
                .map_err(WaitForStopReasonError::Target)?
            {
                return session
                    .stopped(entry)
                    .map(Event::TargetStopped)
                    .map_err(WaitForStopReasonError::Target);
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
            thread::sleep(POLL_TICK);
        }
    }

    /// Ctrl-C halts a running target: `SIGINT`, or the breakpoint or
    /// BGND it reached first.  One while the target is stopped already
    /// is nothing.
    fn on_interrupt(
        session: &mut Session<'t, T>,
    ) -> Result<Option<SingleThreadStopReason<u32>>, T::Error> {
        if session.pending.is_none() {
            return Ok(None);
        }
        match session.target.halt()? {
            Entry::Breakpoint => {
                session.pending = None;
                Ok(Some(SingleThreadStopReason::Signal(Signal::SIGINT)))
            }
            other => session.stopped(other).map(Some),
        }
    }
}
