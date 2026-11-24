//! The core's hook table, and the [`Cpu`] methods that install and
//! call its hooks ([`crate::hooks`]).

use std::fmt;
use std::mem;
use std::ops::RangeInclusive;

use crate::cpu::Core;
use crate::cpu::Cpu;
use crate::cpu::Stop;
use crate::cpu::bare::Intercepted;
use crate::exception::BusCycleKind;
use crate::exception::Frame;
use crate::exception::Vector;
use crate::exception::VectorSet;
use crate::hooks::Control;
use crate::hooks::ExceptionEntry;
use crate::hooks::ExceptionReturn;
use crate::hooks::HookId;
use crate::hooks::Instruction;
use crate::hooks::Invalid;
use crate::hooks::InvalidAccess;
use crate::hooks::MemoryAccess;
use crate::hooks::PendingRead;
use crate::interrupt::InterruptLevel;
use crate::memory::Memory;
use crate::memory::Permissions;
use crate::memory::Space;
use crate::memory::watch;
use mc683xx_target::Address;
use mc683xx_target::FunctionCode;
use mc683xx_target::Size;

#[cfg(test)]
mod tests;

/// A hook's callback, given what it hooks.
type Callback<T> = Box<dyn FnMut(&T) -> Control>;
/// A code or block hook.
type InstructionHook = Callback<Instruction>;
/// A read or write hook.
type MemoryHook = Callback<MemoryAccess>;
/// A pre-read hook: given the read, the core's memory to look at, and
/// the copy of the bytes it is to read that answers it.
type PreReadHook = Box<dyn FnMut(&PendingRead, &Memory, &mut [u8]) -> Control>;
/// An invalid access hook.
type InvalidHook = Callback<InvalidAccess>;
/// An exception hook.
type ExceptionHook = Callback<ExceptionEntry>;
/// An exception return hook.
type ExceptionReturnHook = Callback<ExceptionReturn>;
/// An intercept hook.
type InterceptHook = Callback<Intercepted>;

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
    /// Before each operand read the core's memory serves whole, of a
    /// byte in range.
    pre_read: Vec<Ranged<PreReadHook>>,
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
    /// Before an instruction's exception of one of the vectors is
    /// stacked.
    intercept: Vec<(HookId, VectorSet, InterceptHook)>,
}

impl fmt::Debug for Hooks {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Hooks")
            .field("code", &self.code.len())
            .field("block", &self.block.len())
            .field("pre_read", &self.pre_read.len())
            .field("read", &self.read.len())
            .field("write", &self.write.len())
            .field("invalid", &self.invalid.len())
            .field("exception", &self.exception.len())
            .field("exception_return", &self.exception_return.len())
            .field("intercept", &self.intercept.len())
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
            .chain(ranges(&self.pre_read, watch::PRE_READ))
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
        let intercepts = self.intercept.len();
        self.intercept.retain(|(other, _, _)| *other != id);
        retain(&mut self.code, id)
            | retain(&mut self.block, id)
            | retain(&mut self.pre_read, id)
            | retain(&mut self.read, id)
            | retain(&mut self.write, id)
            | retain(&mut self.invalid, id)
            | retain_unranged(&mut self.exception, id)
            | retain_unranged(&mut self.exception_return, id)
            | (self.intercept.len() != intercepts)
    }

    /// The vectors the intercept hooks cover.
    fn intercepted(&self) -> VectorSet {
        self.intercept
            .iter()
            .fold(VectorSet::default(), |set, (_, vectors, _)| {
                set.union(*vectors)
            })
    }

    /// Calls the intercept hooks covering `intercepted`'s vector, in
    /// the order installed, until one stops the run: whether one did.
    pub(crate) fn intercept(&mut self, intercepted: &Intercepted) -> bool {
        self.intercept
            .iter_mut()
            .filter(|(_, vectors, _)| vectors.contains(intercepted.vector))
            .any(|(_, _, hook)| hook(intercepted).is_break())
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

    /// Calls `hook` once before each operand read in user or supervisor
    /// space, of 1, 2 or 4 bytes, exception processing's included, that
    /// reads a byte in `range` and that the core's memory (RAM or ROM
    /// its pages map) serves all of, with the core's memory to look at
    /// and a copy of the bytes the read is to read: what the hooks leave
    /// in the copy, in the order installed, answers the read, and is not
    /// stored.  A pre-read hook is a device model that answers an
    /// operand, where the read hooks observe bus cycles, so a long is
    /// one call whatever [`set_long_bus`](Self::set_long_bus) says: on
    /// the chip, whose long is two word cycles, one pre-read call and
    /// two read hook calls.  A long that straddles two pages the core's
    /// memory serves is one call too; a long
    /// with a byte the bus serves (a peripheral's, or memory the core's
    /// pages do not map) is no call, its cycles made as usual.  Not for
    /// a fetch, a misaligned word or long (an address error), or a look
    /// with no cycle (`Memory::peek`, a debugger's read), nor a read its
    /// pages' permissions refuse, a fault whose answer would be
    /// discarded.  Its cycles follow, timed as the pages time them; the
    /// read hooks see each with the value the copy gave it.  If it stops the run, the instruction completes first.
    pub fn add_pre_read_hook(
        &mut self,
        range: RangeInclusive<Address>,
        hook: impl FnMut(&PendingRead, &Memory, &mut [u8]) -> Control + 'static,
    ) -> HookId {
        let id = self.hooks.allocate();
        self.hooks.pre_read.push(Ranged {
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

    /// Calls `hook` for each exception an instruction raises with a
    /// vector of `vectors` ([`VectorSet::INSTRUCTION`] or
    /// [`VectorSet::ALL`] for any: a trap, an illegal instruction, a
    /// divide by zero, but not a fault, a trace or an interrupt), before
    /// anything
    /// is stacked.  If it stops the run, the run ends
    /// ([`End::Intercepted`]) with the exception not taken, the
    /// instruction's (An)+ and -(An) undone and PC at it, for the host
    /// to handle (a stubbed kernel call, an emulated instruction) and
    /// run on from PC, or to decline
    /// ([`take_natively_once`](Self::take_natively_once)).  If it does
    /// not, the CPU takes the exception natively.  Where several hooks
    /// cover a vector, they are called in the order installed until one
    /// stops the run; the rest are not called.
    ///
    /// [`End::Intercepted`]: super::bare::End::Intercepted
    pub fn add_intercept_hook(
        &mut self,
        vectors: VectorSet,
        hook: impl FnMut(&Intercepted) -> Control + 'static,
    ) -> HookId {
        let id = self.hooks.allocate();
        self.hooks.intercept.push((id, vectors, Box::new(hook)));
        self.bare.intercepted = self.hooks.intercepted();
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
            || hooks.pre_read.iter().any(|hook| hook.id == id)
            || memory(&hooks.read)
            || memory(&hooks.write)
            || hooks.invalid.iter().any(|hook| hook.id == id)
            || hooks.exception.iter().any(|(other, _)| *other == id)
            || hooks.exception_return.iter().any(|(other, _)| *other == id)
            || hooks.intercept.iter().any(|(other, _, _)| *other == id)
    }

    /// Removes the hook `id`; whether it was installed.
    pub fn remove_hook(&mut self, id: HookId) -> bool {
        let removed = self.hooks.remove(id);
        self.bare.intercepted = self.hooks.intercepted();
        self.rewatch();
        removed
    }

    /// Sets the pages' hook bits from the hooks.
    pub(crate) fn rewatch(&mut self) {
        self.memory.set_watched(self.hooks.watched());
        self.pre_reading = !self.hooks.pre_read.is_empty();
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
    /// it, else run and shown to the read hooks, reading `served` in
    /// place of memory's value if the pre-read hooks answered it.
    #[inline(never)]
    pub(super) fn special_read(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        cycle: BusCycleKind,
        served: Option<u32>,
    ) -> Option<u32> {
        if !self.permitted(address, function_code, cycle) {
            self.invalid(InvalidAccess {
                address,
                size,
                function_code,
                kind: cycle,
                value: None,
                reason: Invalid::Protected,
                pc: self.cpu.instruction_pc,
                stacking: self.cpu.stacking,
            });
            return None;
        }
        let read = self.paged_read(address, size, function_code, cycle)?;
        let value = served.unwrap_or(read);
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
                kind: BusCycleKind::Write,
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

    /// Whether an operand access of `size` at `address` in
    /// `function_code`'s space, a cycle of kind `cycle`, is one the
    /// pre-access hooks whose [`watch`] bit is `bit` may see: in a
    /// memory space, aligned, a byte of it on a page with `bit`, and
    /// allowed by its pages' permissions, as a refused access is a
    /// fault, whose answer would be discarded.
    fn pre_hooked(
        &self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        cycle: BusCycleKind,
        bit: u8,
    ) -> bool {
        if !function_code.is_memory_space() || (size != Size::Byte && address.is_odd()) {
            return false;
        }
        let memory = &self.cpu.memory;
        let first = memory.mask(address);
        let last = memory.mask(address.offset(size.bytes() - 1));
        (memory.watched(first) | memory.watched(last)) & bit != 0
            && self.permitted(first, function_code, cycle)
            && self.permitted(last, function_code, cycle)
    }

    /// The pre-read hooks for an operand read of `size` at `address`
    /// in `function_code`'s space, aligned, that the core's memory serves
    /// all of and its pages' permissions allow: each covering a byte of it, in the order installed, given
    /// the memory and a copy of its bytes.  The value the copy then
    /// holds, which answers the read; [`None`] if no hook was called.
    #[inline(never)]
    pub(super) fn pre_read(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
    ) -> Option<u32> {
        if !self.pre_hooked(
            address,
            size,
            function_code,
            BusCycleKind::Read,
            watch::PRE_READ,
        ) {
            return None;
        }
        let memory = &self.cpu.memory;
        let length = size.bytes();
        let at = |offset: u32| memory.mask(address.offset(offset));
        let space = Space::of(function_code);
        let mut copy = [0; 4];
        let bytes = &mut copy[..length as usize];
        for (offset, byte) in (0..length).zip(bytes.iter_mut()) {
            *byte = memory.peek(space, at(offset), Size::Byte)? as u8;
        }
        let read = PendingRead {
            address: at(0),
            size,
            function_code,
            pc: self.cpu.instruction_pc,
            stacking: self.cpu.stacking,
        };
        let mut called = false;
        let mut stopped = None;
        for hook in self
            .cpu
            .hooks
            .pre_read
            .iter_mut()
            .filter(|hook| (0..length).any(|offset| hook.range.contains(&at(offset))))
        {
            called = true;
            if (hook.hook)(&read, memory, bytes).is_break() {
                stopped.get_or_insert(hook.id);
            }
        }
        if let Some(id) = stopped {
            self.cpu.stop_request.get_or_insert(Stop::Hook(id));
        }
        called.then(|| {
            bytes
                .iter()
                .fold(0, |value, &byte| value << 8 | u32::from(byte))
        })
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
    /// hooks: the bytes the core's memory writes to there, read with no
    /// cycle (not the bytes a page reads, where the two differ), while a
    /// write hook watches the page and the cycle is not part of a long
    /// operand seen once.  [`None`] otherwise, where the
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
            kind: cycle,
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
