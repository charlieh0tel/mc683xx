//! The host's hooks: plain in-process callbacks on what the core does.
//!
//! They are cheap when absent ([PLAN.md, Hooks]).  Code, block and
//! memory hooks cover address ranges, whose pages carry a bit: a cycle
//! or an instruction looks a hook up only on a page with its bit, and
//! only while some page has one.  Any hook can stop the run
//! ([`Stop::Hook`]).
//!
//! [PLAN.md, Hooks]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#hooks

use std::fmt;
use std::mem;
use std::ops::ControlFlow;
use std::ops::RangeInclusive;

use super::Core;
use super::Cpu;
use super::Stop;
use crate::exception::BusCycleKind;
use crate::exception::Frame;
use crate::exception::Vector;
use crate::interrupt::InterruptLevel;
use crate::memory::Permissions;
use crate::memory::Space;
use crate::memory::watch;
use crate::registers::Registers;
use mc683xx_target::address::Address;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::size::Size;

#[cfg(test)]
mod tests;

/// A hook, as the `Cpu::add_*_hook` that installed it returns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HookId(u64);

/// `hook` and its number, in installing order from 1: `hook 3`.
impl fmt::Display for HookId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "hook {}", self.0)
    }
}

/// What a hook returns: [`Continue`](ControlFlow::Continue), or
/// [`Break`](ControlFlow::Break) to stop the run.
pub type Control = ControlFlow<()>;

/// An instruction about to execute, as a code or block hook sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Instruction {
    /// Its address.
    pub pc: Address,
    /// The registers.
    pub registers: Registers,
    /// The long word at A7, in data space, if the core serves it from
    /// memory (no bus cycle is run for it).  A word of data, not an
    /// [`Address`]: it is one only where the code makes it one, as at a
    /// subroutine's first instruction, its return address.
    pub stack_top: Option<u32>,
}

/// A bus cycle in user or supervisor space, as a memory hook sees it.
///
/// A byte or word cycle, once it has ended well.  A long operand is two
/// word cycles, as on the bus, but with [`Cpu::set_long_bus`] one
/// access of size 4 once the whole operand has ended well.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MemoryAccess {
    /// Its address, on the chip's address lines.
    pub address: Address,
    /// Byte, word, or long ([`Cpu::set_long_bus`]).
    pub size: Size,
    /// Its function code.
    pub function_code: FunctionCode,
    /// The value read or written, right-aligned.
    pub value: u32,
    /// For a write, the value it replaced, right-aligned, read with no
    /// cycle before it was written from the bytes it writes to (a
    /// page's write backing, not its read one): [`None`] for a read, for a write in
    /// CPU space, and for a write the core's memory does not serve (a
    /// peripheral's: a read there would be a cycle of its own).
    pub previous: Option<u32>,
    /// The address of the instruction it is part of.
    pub pc: Address,
    /// Whether it is exception processing stacking a frame.
    pub stacking: bool,
}

/// Why a bus cycle was invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Invalid {
    /// The bus ended it in a bus error: nothing answered, or a peripheral
    /// refused it.
    BusError,
    /// Its page's permissions do not allow it
    /// ([`Memory::protect`](crate::memory::Memory::protect)); it ends in a
    /// bus error.
    Protected,
}

/// A bus cycle that ended in a bus error, as an invalid access hook
/// sees it.  The CPU then takes the bus error exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct InvalidAccess {
    /// Its address, on the chip's address lines.
    pub address: Address,
    /// Byte or word.
    pub size: Size,
    /// Its function code.
    pub function_code: FunctionCode,
    /// Fetch, read or write.
    pub cycle: BusCycleKind,
    /// A write's value, right-aligned.
    pub value: Option<u32>,
    /// Why.
    pub reason: Invalid,
    /// The address of the instruction it is part of.
    pub pc: Address,
    /// Whether it is exception processing stacking a frame.
    pub stacking: bool,
}

/// An exception entered: its frame stacked and its handler's address
/// in PC (CPU32RM 6.1.3).  Reset is not one: the chip reports resets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExceptionEntry {
    /// Its vector.
    pub vector: Vector,
    /// The return PC its frame stacked.
    pub pc: Address,
    /// Its handler's address.
    pub handler: Address,
    /// SSP: its frame's lowest address.
    pub stack: Address,
    /// The level, for an interrupt.
    pub level: Option<InterruptLevel>,
}

/// An exception returned from: `rte` restored SR and PC from its four-
/// or six-word frame, or from a format $C frame of a bus or address
/// error in an instruction, and deallocated it (CPU32RM 6.2.12).  A
/// format $C frame of a fault in exception processing (6.3.1.4) is not
/// one: its `rte` takes the faulted exception again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExceptionReturn {
    /// The vector its frame's format/vector word names.
    pub vector: Vector,
    /// The PC it returns to.
    pub pc: Address,
    /// The SR it restored.
    pub sr: u16,
    /// Its frame's lowest address, SSP as `rte` began: an
    /// [`ExceptionEntry`]'s `stack` for the same frame.
    pub stack: Address,
}

/// A hook's callback, given what it hooks.
type Callback<T> = Box<dyn FnMut(&T) -> Control>;
/// A code or block hook.
type InstructionHook = Callback<Instruction>;
/// A read or write hook.
type MemoryHook = Callback<MemoryAccess>;
/// An invalid access hook.
type InvalidHook = Callback<InvalidAccess>;
/// An exception hook.
type ExceptionHook = Callback<ExceptionEntry>;
/// An exception return hook.
type ExceptionReturnHook = Callback<ExceptionReturn>;

/// A hook with the addresses it covers.
struct Ranged<H> {
    /// Its identity.
    id: HookId,
    /// The addresses, on the chip's address lines.
    range: RangeInclusive<Address>,
    /// The callback.
    hook: H,
}

/// Calls each of `hooks` covering `address` with `context`, in the
/// order installed; the first that stopped, if any did.
fn call<T>(hooks: &mut [Ranged<Callback<T>>], address: Address, context: &T) -> Option<HookId> {
    let mut stopped = None;
    for hook in hooks
        .iter_mut()
        .filter(|hook| hook.range.contains(&address))
    {
        if (hook.hook)(context).is_break() {
            stopped.get_or_insert(hook.id);
        }
    }
    stopped
}

/// Where a run a code or block hook stopped resumes: before the hooks
/// that already ran for the instruction at that address, once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Resume {
    /// A block hook stopped: the block hooks are skipped.
    Block,
    /// A code hook stopped: the block and code hooks are skipped.
    Code,
}

/// The hooks installed, each kind in the order installed.
#[derive(Default)]
pub(crate) struct Hooks {
    /// The next [`HookId`].
    next: u64,
    /// Before each instruction in range.
    code: Vec<Ranged<InstructionHook>>,
    /// Before each instruction in range that starts a block: the first
    /// after a change of flow, an exception, or reset.
    block: Vec<Ranged<InstructionHook>>,
    /// After each read cycle in range but fetches.
    read: Vec<Ranged<MemoryHook>>,
    /// After each write cycle in range.
    write: Vec<Ranged<MemoryHook>>,
    /// After each cycle in range that ends in a bus error.
    invalid: Vec<Ranged<InvalidHook>>,
    /// After each exception is entered.
    exception: Vec<(HookId, ExceptionHook)>,
    /// After each exception is returned from.
    exception_return: Vec<(HookId, ExceptionReturnHook)>,
}

impl fmt::Debug for Hooks {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Hooks")
            .field("code", &self.code.len())
            .field("block", &self.block.len())
            .field("read", &self.read.len())
            .field("write", &self.write.len())
            .field("invalid", &self.invalid.len())
            .field("exception", &self.exception.len())
            .field("exception_return", &self.exception_return.len())
            .finish_non_exhaustive()
    }
}

impl Hooks {
    /// A new hook's identity.
    const fn allocate(&mut self) -> HookId {
        self.next += 1;
        HookId(self.next)
    }

    /// The ranges the code, block and memory hooks cover, with their
    /// [`watch`] bits.
    fn watched(&self) -> impl Iterator<Item = (RangeInclusive<Address>, u8)> + '_ {
        fn ranges<H>(
            hooks: &[Ranged<H>],
            bit: u8,
        ) -> impl Iterator<Item = (RangeInclusive<Address>, u8)> + '_ {
            hooks.iter().map(move |hook| (hook.range.clone(), bit))
        }
        ranges(&self.code, watch::CODE)
            .chain(ranges(&self.block, watch::BLOCK))
            .chain(ranges(&self.read, watch::READ))
            .chain(ranges(&self.write, watch::WRITE))
    }

    /// Whether a hook reads the registers as the instruction began.
    pub(crate) const fn want_registers(&self) -> bool {
        !self.read.is_empty() || !self.write.is_empty() || !self.invalid.is_empty()
    }

    /// Removes the hook `id`; whether there was one.
    fn remove(&mut self, id: HookId) -> bool {
        fn retain<H>(hooks: &mut Vec<Ranged<H>>, id: HookId) -> bool {
            let before = hooks.len();
            hooks.retain(|hook| hook.id != id);
            hooks.len() != before
        }
        fn retain_unranged<H>(hooks: &mut Vec<(HookId, H)>, id: HookId) -> bool {
            let before = hooks.len();
            hooks.retain(|(other, _)| *other != id);
            hooks.len() != before
        }
        retain(&mut self.code, id)
            | retain(&mut self.block, id)
            | retain(&mut self.read, id)
            | retain(&mut self.write, id)
            | retain(&mut self.invalid, id)
            | retain_unranged(&mut self.exception, id)
            | retain_unranged(&mut self.exception_return, id)
    }
}

impl Cpu {
    /// Calls `hook` before each instruction at an address in `range`,
    /// with the registers.  If it stops the run, the instruction has
    /// not executed; the next run executes it without calling the code
    /// hooks for it again.
    pub fn add_code_hook(
        &mut self,
        range: RangeInclusive<Address>,
        hook: impl FnMut(&Instruction) -> Control + 'static,
    ) -> HookId {
        let id = self.hooks.allocate();
        self.hooks.code.push(Ranged {
            id,
            range,
            hook: Box::new(hook),
        });
        self.rewatch();
        id
    }

    /// Calls `hook` before the first instruction of each block whose
    /// address is in `range`: straight-line code from a change of flow,
    /// an exception or reset to the next.  It stops the run as a code
    /// hook does, before the code hooks.
    pub fn add_block_hook(
        &mut self,
        range: RangeInclusive<Address>,
        hook: impl FnMut(&Instruction) -> Control + 'static,
    ) -> HookId {
        let id = self.hooks.allocate();
        self.hooks.block.push(Ranged {
            id,
            range,
            hook: Box::new(hook),
        });
        self.rewatch();
        id
    }

    /// Calls `hook` after each operand read cycle in user or supervisor
    /// space at an address in `range`, exception processing's
    /// included; instruction fetches are for code hooks.  If it stops
    /// the run, the instruction completes first.
    pub fn add_read_hook(
        &mut self,
        range: RangeInclusive<Address>,
        hook: impl FnMut(&MemoryAccess) -> Control + 'static,
    ) -> HookId {
        let id = self.hooks.allocate();
        self.hooks.read.push(Ranged {
            id,
            range,
            hook: Box::new(hook),
        });
        self.rewatch();
        id
    }

    /// Calls `hook` after each write cycle in user or supervisor space
    /// at an address in `range`, as `add_read_hook`.
    pub fn add_write_hook(
        &mut self,
        range: RangeInclusive<Address>,
        hook: impl FnMut(&MemoryAccess) -> Control + 'static,
    ) -> HookId {
        let id = self.hooks.allocate();
        self.hooks.write.push(Ranged {
            id,
            range,
            hook: Box::new(hook),
        });
        self.rewatch();
        id
    }

    /// Calls `hook` for each cycle at an address in `range` that ends
    /// in a bus error, before the CPU takes the exception.  If it stops
    /// the run, the exception is taken first.
    pub fn add_invalid_hook(
        &mut self,
        range: RangeInclusive<Address>,
        hook: impl FnMut(&InvalidAccess) -> Control + 'static,
    ) -> HookId {
        let id = self.hooks.allocate();
        self.hooks.invalid.push(Ranged {
            id,
            range,
            hook: Box::new(hook),
        });
        self.update_capture();
        id
    }

    /// Calls `hook` after each exception is entered.  If it stops the
    /// run, it stops with the handler not yet run.
    pub fn add_exception_hook(
        &mut self,
        hook: impl FnMut(&ExceptionEntry) -> Control + 'static,
    ) -> HookId {
        let id = self.hooks.allocate();
        self.hooks.exception.push((id, Box::new(hook)));
        id
    }

    /// Calls `hook` after each exception is returned from: after `rte`
    /// restored SR and PC from the exception's frame.  If it stops the
    /// run, it stops before the next instruction: the one at the return
    /// PC, or, where the restored SR has T1 or T0 set, the trace
    /// handler's, as `rte` takes the trace exception in the same step
    /// (CPU32RM 6.2.10).
    pub fn add_exception_return_hook(
        &mut self,
        hook: impl FnMut(&ExceptionReturn) -> Control + 'static,
    ) -> HookId {
        let id = self.hooks.allocate();
        self.hooks.exception_return.push((id, Box::new(hook)));
        id
    }

    /// Whether the hook `id` is installed.
    #[must_use]
    pub fn has_hook(&self, id: HookId) -> bool {
        let hooks = &self.hooks;
        let ranged = |list: &[Ranged<InstructionHook>]| list.iter().any(|hook| hook.id == id);
        let memory = |list: &[Ranged<MemoryHook>]| list.iter().any(|hook| hook.id == id);
        ranged(&hooks.code)
            || ranged(&hooks.block)
            || memory(&hooks.read)
            || memory(&hooks.write)
            || hooks.invalid.iter().any(|hook| hook.id == id)
            || hooks.exception.iter().any(|(other, _)| *other == id)
            || hooks.exception_return.iter().any(|(other, _)| *other == id)
    }

    /// Removes the hook `id`; whether it was installed.
    pub fn remove_hook(&mut self, id: HookId) -> bool {
        let removed = self.hooks.remove(id);
        self.rewatch();
        removed
    }

    /// Sets the pages' hook bits from the hooks.
    pub(crate) fn rewatch(&mut self) {
        self.memory.set_watched(self.hooks.watched());
        self.update_capture();
    }

    /// The code and block hooks for the instruction at PC, on a page
    /// that has them: the first that stopped the run.
    #[inline(never)]
    pub(super) fn before_instruction(&mut self) -> Option<HookId> {
        let pc = self.registers.pc;
        let at = self.memory.mask(pc);
        let watched = self.memory.watched(at);
        let resume = self.resume.take().filter(|&(address, _)| address == pc);
        let block_start = mem::take(&mut self.block_start);
        if watched & (watch::CODE | watch::BLOCK) == 0 {
            return None;
        }
        let instruction = || Instruction {
            pc,
            registers: self.registers.clone(),
            stack_top: self.memory.peek(
                Space::of(self.registers.data_space()),
                Address(self.registers.address(7)),
                Size::Long,
            ),
        };
        let block = block_start && watched & watch::BLOCK != 0 && resume.is_none();
        let code =
            watched & watch::CODE != 0 && resume.is_none_or(|(_, stage)| stage == Resume::Block);
        if !block && !code {
            return None;
        }
        let instruction = instruction();
        if block && let Some(id) = call(&mut self.hooks.block, at, &instruction) {
            self.resume = Some((pc, Resume::Block));
            return Some(id);
        }
        if code && let Some(id) = call(&mut self.hooks.code, at, &instruction) {
            self.resume = Some((pc, Resume::Code));
            return Some(id);
        }
        None
    }
}

impl Core<'_> {
    /// A read cycle in a memory space while some page has a hook or
    /// permissions: refused if its page's permissions do not allow
    /// it, else run and shown to the read hooks.
    #[inline(never)]
    pub(super) fn special_read(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        cycle: BusCycleKind,
    ) -> Option<u32> {
        if !self.permitted(address, function_code, cycle) {
            self.invalid(InvalidAccess {
                address,
                size,
                function_code,
                cycle,
                value: None,
                reason: Invalid::Protected,
                pc: self.cpu.instruction_pc,
                stacking: self.cpu.stacking,
            });
            return None;
        }
        let value = self.paged_read(address, size, function_code, cycle)?;
        if cycle != BusCycleKind::Fetch {
            self.watch(
                watch::READ,
                self.memory_access(address, size, function_code, value, None),
            );
        }
        Some(value)
    }

    /// A write cycle in a memory space while some page has a hook or
    /// permissions, as `special_read`.
    #[inline(never)]
    pub(super) fn special_write(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        value: u32,
    ) -> Option<()> {
        if !self.permitted(address, function_code, BusCycleKind::Write) {
            self.invalid(InvalidAccess {
                address,
                size,
                function_code,
                cycle: BusCycleKind::Write,
                value: Some(value),
                reason: Invalid::Protected,
                pc: self.cpu.instruction_pc,
                stacking: self.cpu.stacking,
            });
            return None;
        }
        let previous = self.previous(address, size, function_code);
        self.paged_write(address, size, function_code, value)?;
        self.watch(
            watch::WRITE,
            self.memory_access(address, size, function_code, value, previous),
        );
        Some(())
    }

    /// Whether the page's permissions in `function_code`'s space allow
    /// a cycle of kind `cycle` at `address`.
    fn permitted(
        &self,
        address: Address,
        function_code: FunctionCode,
        cycle: BusCycleKind,
    ) -> bool {
        let needed = match cycle {
            BusCycleKind::Fetch => Permissions::EXECUTE,
            BusCycleKind::Read => Permissions::READ,
            BusCycleKind::Write => Permissions::WRITE,
        };
        self.cpu
            .memory
            .permissions(Space::of(function_code), address)
            .contains(needed)
    }

    /// The read or write hooks, `bit`, for a cycle that ended well, on
    /// a page that has them.
    fn watch(&mut self, bit: u8, access: MemoryAccess) {
        if self.cpu.long_operand || self.cpu.memory.watched(access.address) & bit == 0 {
            return;
        }
        let hooks = if bit == watch::READ {
            &mut self.cpu.hooks.read
        } else {
            &mut self.cpu.hooks.write
        };
        if let Some(id) = call(hooks, access.address, &access) {
            self.cpu.stop_request.get_or_insert(Stop::Hook(id));
        }
    }

    /// The read or write hooks, `bit`, for a long operand seen once
    /// ([`Cpu::set_long_bus`]), a write's `previous` value as
    /// [`previous`](Self::previous) found it.
    pub(super) fn watch_long(
        &mut self,
        bit: u8,
        address: Address,
        function_code: FunctionCode,
        value: u32,
        previous: Option<u32>,
    ) {
        self.watch(
            bit,
            self.memory_access(address, Size::Long, function_code, value, previous),
        );
    }

    /// What a write of `size` at `address` is to replace, for the write
    /// hooks: the bytes the core's memory writes to there, read with
    /// no cycle (not the bytes a page reads, where the two differ), while a write hook watches the page and the cycle is not
    /// part of a long operand seen once.  [`None`] otherwise, where the
    /// memory does not serve the address, for a word or long at an odd
    /// address, which ends in an address error without writing, and for
    /// a cycle in CPU space, which replaces no memory.
    pub(super) fn previous(
        &self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
    ) -> Option<u32> {
        if self.cpu.long_operand
            || !function_code.is_memory_space()
            || (size != Size::Byte && address.is_odd())
            || self.cpu.memory.watched(address) & watch::WRITE == 0
        {
            return None;
        }
        self.cpu
            .memory
            .peek_written(Space::of(function_code), address, size)
    }

    /// A cycle that ended well, as the read and write hooks see it.
    const fn memory_access(
        &self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        value: u32,
        previous: Option<u32>,
    ) -> MemoryAccess {
        MemoryAccess {
            address,
            size,
            function_code,
            value,
            previous,
            pc: self.cpu.instruction_pc,
            stacking: self.cpu.stacking,
        }
    }

    /// A cycle that ended in a bus error, as the invalid access hooks
    /// see it.
    pub(super) const fn invalid_access(
        &self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        cycle: BusCycleKind,
        value: Option<u32>,
    ) -> InvalidAccess {
        InvalidAccess {
            address,
            size,
            function_code,
            cycle,
            value,
            reason: Invalid::BusError,
            pc: self.cpu.instruction_pc,
            stacking: self.cpu.stacking,
        }
    }

    /// The invalid access hooks for a cycle that ended in a bus error.
    #[cold]
    pub(super) fn invalid(&mut self, access: InvalidAccess) {
        self.cpu.bare.last_invalid = Some(access.reason);
        if let Some(id) = call(&mut self.cpu.hooks.invalid, access.address, &access) {
            self.cpu.stop_request.get_or_insert(Stop::Hook(id));
        }
    }

    /// An exception entered: its `frame` stacked at `stack`, PC at its
    /// `handler`.  The next instruction starts a block.
    pub(crate) fn entered(
        &mut self,
        vector: Vector,
        frame: &Frame,
        stack: Address,
        handler: Address,
        level: Option<InterruptLevel>,
    ) {
        self.cpu.registers.ssp = stack;
        self.cpu.registers.pc = handler;
        self.cpu.block_start = true;
        let words = frame.words();
        // Every CPU32 frame stacks its return PC after SR (CPU32RM 6.4).
        let entry = ExceptionEntry {
            vector,
            pc: Address(u32::from(words[1]) << 16 | u32::from(words[2])),
            handler,
            stack,
            level,
        };
        self.bus.exception_entered(&entry, self.now);
        for (id, hook) in &mut self.cpu.hooks.exception {
            if hook(&entry).is_break() {
                self.cpu.stop_request.get_or_insert(Stop::Hook(*id));
            }
        }
    }

    /// An exception of `vector` returned from, its frame at `stack`
    /// deallocated, SR and PC restored.
    pub(crate) fn returned(&mut self, vector: Vector, stack: Address) {
        let registers = &self.cpu.registers;
        let exception_return = ExceptionReturn {
            vector,
            pc: registers.pc,
            sr: registers.sr(),
            stack,
        };
        for (id, hook) in &mut self.cpu.hooks.exception_return {
            if hook(&exception_return).is_break() {
                self.cpu.stop_request.get_or_insert(Stop::Hook(*id));
            }
        }
    }
}
