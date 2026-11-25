//! The core's hook table, and the [`Cpu`] methods that install and
//! call its hooks ([`crate::hooks`]).

use std::fmt;
use std::mem;
use std::ops::RangeInclusive;

use crate::cpu::Core;
use crate::cpu::Cpu;
use crate::cpu::NO_PAGE;
use crate::cpu::Stop;
use crate::cpu::bare::Intercepted;
use crate::exception::BusCycleKind;
use crate::exception::Frame;
use crate::exception::Vector;
use crate::exception::VectorSet;
use crate::hooks::Control;
use crate::hooks::Coverage;
use crate::hooks::CoverageId;
use crate::hooks::CoverageKind;
use crate::hooks::ExceptionEntry;
use crate::hooks::ExceptionReturn;
use crate::hooks::HookId;
use crate::hooks::Instruction;
use crate::hooks::Invalid;
use crate::hooks::InvalidAccess;
use crate::hooks::MemoryAccess;
use crate::hooks::PendingRead;
use crate::hooks::PendingWrite;
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
/// A pre-write hook: given the write, which it may drop, the core's
/// memory to look at, and the copy of the bytes it is to write that
/// it stores.
type PreWriteHook = Box<dyn FnMut(&mut PendingWrite, &Memory, &mut [u8]) -> Control>;
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

/// The opcodes an opcode hook is called for: those whose first word,
/// masked by `mask`, is `value`.
#[derive(Debug, Clone, Copy)]
struct Opcodes {
    /// The bits compared.
    mask: u16,
    /// What they must be.
    value: u16,
}

impl Opcodes {
    /// Whether `opcode`, if known, is one of them.
    fn matches(self, opcode: Option<u16>) -> bool {
        opcode.is_some_and(|opcode| opcode & self.mask == self.value)
    }
}

/// A code hook: its callback, called for every instruction in its
/// range or, an opcode hook's, for those of some opcodes.
struct CodeHook {
    /// The opcodes, an opcode hook's; [`None`] for every instruction.
    opcodes: Option<Opcodes>,
    /// The callback.
    callback: InstructionHook,
}

impl CodeHook {
    /// Its [`watch`] bit.
    const fn watch_bit(&self) -> watch::Bits {
        if self.opcodes.is_some() {
            watch::OPCODE
        } else {
            watch::CODE
        }
    }

    /// Whether it is called for an instruction whose first word is
    /// `opcode`, if known.
    fn wants(&self, opcode: Option<u16>) -> bool {
        self.opcodes.is_none_or(|opcodes| opcodes.matches(opcode))
    }
}

/// What the pre-write hooks made of an operand write.
pub(super) enum PreWrite {
    /// Store this value.
    Store(u32),
    /// Store nothing: a hook dropped it.
    Dropped,
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

/// The [`watch`] bit of a coverage of `kind`.
const fn coverage_bit(kind: CoverageKind) -> watch::Bits {
    match kind {
        CoverageKind::Execute => watch::EXECUTE_COVERAGE,
        CoverageKind::Read => watch::READ_COVERAGE,
        CoverageKind::Write => watch::WRITE_COVERAGE,
    }
}

/// What [`Hooks::remove`] took out.
enum Removed {
    /// No hook had the id.
    Nothing,
    /// A hook that sets no [`watch`] bit.
    Unwatched,
    /// A hook over a range, with its [`watch`] bit.
    Watched(RangeInclusive<Address>, watch::Bits),
}

/// The hooks installed, each kind in the order installed.
#[derive(Default)]
pub(crate) struct Hooks {
    /// The next [`HookId`].
    next: u64,
    /// Before each instruction in range, or each of some opcodes.
    code: Vec<Ranged<CodeHook>>,
    /// Before each instruction in range that starts a block: the first
    /// after a change of flow, an exception, or reset.
    block: Vec<Ranged<InstructionHook>>,
    /// Before each operand read the core's memory serves whole, of a
    /// byte in range.
    pre_read: Vec<Ranged<PreReadHook>>,
    /// Before each operand write the core's memory stores whole, of a
    /// byte in range.
    pre_write: Vec<Ranged<PreWriteHook>>,
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
    /// The next [`CoverageId`].
    next_coverage: u64,
    /// The coverages, in the order added, each with the range it was
    /// added with and its instructions executed.
    coverage: Vec<(CoverageId, RangeInclusive<Address>, Coverage)>,
}

impl fmt::Debug for Hooks {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Hooks")
            .field("code", &self.code.len())
            .field("block", &self.block.len())
            .field("pre_read", &self.pre_read.len())
            .field("pre_write", &self.pre_write.len())
            .field("read", &self.read.len())
            .field("write", &self.write.len())
            .field("invalid", &self.invalid.len())
            .field("exception", &self.exception.len())
            .field("exception_return", &self.exception_return.len())
            .field("intercept", &self.intercept.len())
            .field("coverage", &self.coverage.len())
            .finish_non_exhaustive()
    }
}

impl Hooks {
    /// A new hook's identity.
    const fn allocate(&mut self) -> HookId {
        self.next += 1;
        HookId(self.next)
    }

    /// Whether a hook reads the registers as the instruction began.
    pub(crate) const fn want_registers(&self) -> bool {
        !self.read.is_empty() || !self.write.is_empty() || !self.invalid.is_empty()
    }

    /// Removes the hook `id`: what it was, if there was one.
    fn remove(&mut self, id: HookId) -> Removed {
        fn take<H>(hooks: &mut Vec<Ranged<H>>, id: HookId) -> Option<RangeInclusive<Address>> {
            let index = hooks.iter().position(|hook| hook.id == id)?;
            Some(hooks.remove(index).range)
        }
        fn take_unranged<T>(hooks: &mut Vec<T>, id: HookId, of: impl Fn(&T) -> HookId) -> bool {
            let before = hooks.len();
            hooks.retain(|hook| of(hook) != id);
            hooks.len() != before
        }
        if let Some(index) = self.code.iter().position(|hook| hook.id == id) {
            let hook = self.code.remove(index);
            return Removed::Watched(hook.range, hook.hook.watch_bit());
        }
        let watched = [
            (take(&mut self.block, id), watch::BLOCK),
            (take(&mut self.pre_read, id), watch::PRE_READ),
            (take(&mut self.pre_write, id), watch::PRE_WRITE),
            (take(&mut self.read, id), watch::READ),
            (take(&mut self.write, id), watch::WRITE),
        ];
        if let Some((range, bit)) = watched
            .into_iter()
            .find_map(|(range, bit)| range.map(|range| (range, bit)))
        {
            return Removed::Watched(range, bit);
        }
        let unwatched = take(&mut self.invalid, id).is_some()
            | take_unranged(&mut self.exception, id, |(other, _)| *other)
            | take_unranged(&mut self.exception_return, id, |(other, _)| *other)
            | take_unranged(&mut self.intercept, id, |(other, _, _)| *other);
        if unwatched {
            Removed::Unwatched
        } else {
            Removed::Nothing
        }
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
        self.add_filtered_code_hook(range, None, Box::new(hook))
    }

    /// Calls `hook` as a code hook does, but only before each
    /// instruction at an address in `range` whose first word, the
    /// opcode, masked by `mask`, is `value` (`0xfff0`, `0x4e40` for
    /// every TRAP).  The opcode is the word the core's memory serves at
    /// PC, read with no cycle, so an instruction the bus would supply (a
    /// peripheral's) never matches.  The filter runs natively, before
    /// the registers are copied for a call: an instruction it does not
    /// match costs a lookup and a compare, and a page with no opcode
    /// hook nothing.  Installed with the code hooks, it is called in
    /// their order and stops the run as they do.
    pub fn add_opcode_hook(
        &mut self,
        range: RangeInclusive<Address>,
        mask: u16,
        value: u16,
        hook: impl FnMut(&Instruction) -> Control + 'static,
    ) -> HookId {
        let opcodes = Opcodes { mask, value };
        self.add_filtered_code_hook(range, Some(opcodes), Box::new(hook))
    }

    /// Installs a code hook, an opcode hook if it has `opcodes`.
    fn add_filtered_code_hook(
        &mut self,
        range: RangeInclusive<Address>,
        opcodes: Option<Opcodes>,
        callback: InstructionHook,
    ) -> HookId {
        let id = self.hooks.allocate();
        let hook = CodeHook { opcodes, callback };
        self.memory.add_watch(range.clone(), hook.watch_bit());
        self.hooks.code.push(Ranged { id, range, hook });
        self.hooks_changed();
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
        self.memory.add_watch(range.clone(), watch::BLOCK);
        self.hooks.block.push(Ranged {
            id,
            range,
            hook: Box::new(hook),
        });
        self.hooks_changed();
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
    /// read hooks see each with the value the copy gave it.  Its pages
    /// leave the fast path for reads, fetches included, not writes.  If
    /// it stops the run, the instruction completes first.
    pub fn add_pre_read_hook(
        &mut self,
        range: RangeInclusive<Address>,
        hook: impl FnMut(&PendingRead, &Memory, &mut [u8]) -> Control + 'static,
    ) -> HookId {
        let id = self.hooks.allocate();
        self.memory.add_watch(range.clone(), watch::PRE_READ);
        self.hooks.pre_read.push(Ranged {
            id,
            range,
            hook: Box::new(hook),
        });
        self.hooks_changed();
        id
    }

    /// Calls `hook` once before each operand write in user or supervisor
    /// space, of 1, 2 or 4 bytes, exception processing's stacking
    /// included, that writes a byte in `range` and that the core's
    /// memory (RAM its pages map) stores all of, with the write, the
    /// core's memory to look at and a copy of the bytes it is to write:
    /// what the hooks leave in the copy, in the order installed, is
    /// stored, unless one dropped the store
    /// ([`PendingWrite::set_dropped`]), which each later hook sees and
    /// may undo.  As a pre-read hook, a device model that takes an
    /// operand, so a long is one call whatever
    /// [`set_long_bus`](Self::set_long_bus) says, one across two pages
    /// the core's memory stores too; a long with a byte the bus takes
    /// (a peripheral's, ROM, or memory the core's pages do not map) is
    /// no call.  Not for a misaligned word or long (an address error),
    /// a write its pages' permissions refuse (a fault), or a write with
    /// no cycle (`Memory::write_bytes`, a debugger's).  The memory is
    /// as it was before the store: the hook cannot change it.  Its
    /// cycles follow, timed as the pages time them; the write hooks see
    /// each with the value the copy gave it and, as `previous`, what it
    /// replaced.  A dropped store keeps its wait states, as the CPU
    /// still drove the cycles and the device the hook models latched
    /// them, but writes nothing, and the write hooks see none of them.  Its pages leave
    /// the fast path for writes, not reads.  If it stops the run, the
    /// instruction completes first.
    pub fn add_pre_write_hook(
        &mut self,
        range: RangeInclusive<Address>,
        hook: impl FnMut(&mut PendingWrite, &Memory, &mut [u8]) -> Control + 'static,
    ) -> HookId {
        let id = self.hooks.allocate();
        self.memory.add_watch(range.clone(), watch::PRE_WRITE);
        self.hooks.pre_write.push(Ranged {
            id,
            range,
            hook: Box::new(hook),
        });
        self.hooks_changed();
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
        self.memory.add_watch(range.clone(), watch::READ);
        self.hooks.read.push(Ranged {
            id,
            range,
            hook: Box::new(hook),
        });
        self.hooks_changed();
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
        self.memory.add_watch(range.clone(), watch::WRITE);
        self.hooks.write.push(Ranged {
            id,
            range,
            hook: Box::new(hook),
        });
        self.hooks_changed();
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

    /// Records in a new coverage what `range`, on the address lines,
    /// sees of `kind`:
    ///
    /// - [`CoverageKind::Execute`]: the address of each instruction that
    ///   starts in it as it executes, once no code or block hook stopped
    ///   the run before it, a bit for each word.
    /// - [`CoverageKind::Read`]: each byte of each operand read cycle a
    ///   read hook over it would see ([`add_read_hook`]), a bit for each
    ///   byte: memory's or a peripheral's, in user or supervisor space,
    ///   exception processing's (vectors, `rte`'s frame) included, once
    ///   it ended well; not a fetch, a cycle in CPU space, or one that
    ///   faulted.  A long seen once ([`set_long_bus`]) is its four bytes.
    /// - [`CoverageKind::Write`]: likewise each byte of each write cycle
    ///   a write hook would see, exception stacking included; not a store
    ///   a pre-write hook dropped.
    ///
    /// A page's bits are allocated when the first is set, so a coverage
    /// holds what it recorded, not its range.  The start is rounded up
    /// to a word for an execute coverage, the end clamped to the address
    /// lines; a range starting past either is empty.  An instruction or
    /// a cycle outside every coverage's pages costs what it did; an
    /// instruction on an execute coverage's costs a lookup, and a read
    /// or write coverage's pages leave the fast path for reads (fetches
    /// included) or writes alone.  Not part of a snapshot, which neither
    /// saves nor restores it, as the hooks.
    ///
    /// [`add_read_hook`]: Self::add_read_hook
    /// [`set_long_bus`]: Self::set_long_bus
    pub fn add_coverage(
        &mut self,
        range: RangeInclusive<Address>,
        kind: CoverageKind,
    ) -> CoverageId {
        self.hooks.next_coverage += 1;
        let id = CoverageId(self.hooks.next_coverage);
        let coverage = Coverage::new(&range, self.memory.address_mask, kind);
        self.memory.add_watch(range.clone(), coverage_bit(kind));
        self.hooks.coverage.push((id, range, coverage));
        self.hooks_changed();
        id
    }

    /// Coverage `id`, if it was added and not removed.
    #[must_use]
    pub fn coverage(&self, id: CoverageId) -> Option<&Coverage> {
        self.hooks
            .coverage
            .iter()
            .find_map(|(other, _, coverage)| (*other == id).then_some(coverage))
    }

    /// Forgets what coverage `id` recorded; whether there
    /// is one.
    pub fn clear_coverage(&mut self, id: CoverageId) -> bool {
        self.hooks
            .coverage
            .iter_mut()
            .find(|(other, _, _)| *other == id)
            .map(|(_, _, coverage)| coverage.clear())
            .is_some()
    }

    /// Removes coverage `id`: what it recorded, if there was one.
    pub fn remove_coverage(&mut self, id: CoverageId) -> Option<Coverage> {
        let index = self
            .hooks
            .coverage
            .iter()
            .position(|(other, _, _)| *other == id)?;
        let (_, range, coverage) = self.hooks.coverage.remove(index);
        self.memory
            .remove_watch(&range, coverage_bit(coverage.kind()));
        self.hooks_changed();
        Some(coverage)
    }

    /// Whether the hook `id` is installed.
    #[must_use]
    pub fn has_hook(&self, id: HookId) -> bool {
        let hooks = &self.hooks;
        let ranged = |list: &[Ranged<InstructionHook>]| list.iter().any(|hook| hook.id == id);
        let memory = |list: &[Ranged<MemoryHook>]| list.iter().any(|hook| hook.id == id);
        hooks.code.iter().any(|hook| hook.id == id)
            || ranged(&hooks.block)
            || hooks.pre_read.iter().any(|hook| hook.id == id)
            || hooks.pre_write.iter().any(|hook| hook.id == id)
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
        if let Removed::Watched(range, bit) = &removed {
            self.memory.remove_watch(range, *bit);
        }
        self.bare.intercepted = self.hooks.intercepted();
        self.hooks_changed();
        !matches!(removed, Removed::Nothing)
    }

    /// Brings what follows the hooks up to date, their pages' bits
    /// having been: the quiet page forgotten, the flags of the
    /// pre-access hooks, and whether the registers are captured.
    fn hooks_changed(&mut self) {
        self.quiet_page = NO_PAGE;
        self.pre_reading = !self.hooks.pre_read.is_empty();
        self.pre_writing = !self.hooks.pre_write.is_empty();
        self.update_capture();
    }

    /// The code and block hooks for the instruction at PC, while some
    /// page has a hook or permissions: none on the quiet page, which
    /// only forgets the block start and the resume, as
    /// [`before_instruction`](Self::before_instruction) does there.
    /// Always inlined, so an instruction on it costs a compare.
    #[inline(always)]
    pub(super) fn hooks_before_instruction(&mut self) -> Option<HookId> {
        if self.memory.page_number(self.registers.pc) == self.quiet_page {
            self.block_start = false;
            self.resume = None;
            return None;
        }
        self.before_instruction()
    }

    /// The code and block hooks for the instruction at PC, on a page
    /// that has them: the first that stopped the run.  A page with none
    /// becomes the quiet page.
    #[inline(never)]
    fn before_instruction(&mut self) -> Option<HookId> {
        let pc = self.registers.pc;
        let at = self.memory.mask(pc);
        let watched = self.memory.watched(at);
        let resume = self.resume.take().filter(|&(address, _)| address == pc);
        let block_start = mem::take(&mut self.block_start);
        if watched & (watch::CODE | watch::OPCODE | watch::BLOCK | watch::EXECUTE_COVERAGE) == 0 {
            self.quiet_page = self.memory.page_number(at);
            return None;
        }
        let block = block_start && watched & watch::BLOCK != 0 && resume.is_none();
        let opcode = if watched & watch::OPCODE != 0 {
            self.opcode()
        } else {
            None
        };
        // an opcode hook's page: the registers are copied for a match alone
        let code = resume.is_none_or(|(_, stage)| stage == Resume::Block)
            && (watched & watch::CODE != 0
                || self.hooks.code.iter().any(|hook| {
                    hook.hook.opcodes.is_some()
                        && hook.range.contains(&at)
                        && hook.hook.wants(opcode)
                }));
        if block || code {
            let instruction = Instruction {
                pc,
                instruction: self.instructions,
                registers: self.registers.clone(),
                stack_top: self.memory.peek(
                    Space::of(self.registers.data_space()),
                    Address(self.registers.address(7)),
                    Size::Long,
                ),
                opcode: opcode.or_else(|| self.opcode()),
            };
            if block && let Some(id) = call(&mut self.hooks.block, at, &instruction) {
                self.resume = Some((pc, Resume::Block));
                return Some(id);
            }
            if code && let Some(id) = self.call_code_hooks(at, &instruction) {
                self.resume = Some((pc, Resume::Code));
                return Some(id);
            }
        }
        if watched & watch::EXECUTE_COVERAGE != 0 {
            for (_, _, coverage) in &mut self.hooks.coverage {
                if coverage.kind() == CoverageKind::Execute {
                    coverage.record(at);
                }
            }
        }
        None
    }
}

impl Cpu {
    /// The word at PC in program space, if the core's memory serves it
    /// and PC is even: the opcode the next instruction starts with.
    fn opcode(&self) -> Option<u16> {
        let pc = self.registers.pc;
        if pc.is_odd() {
            return None;
        }
        let space = Space::of(self.registers.program_space());
        self.memory
            .peek(space, pc, Size::Word)
            .map(|word| word as u16)
    }

    /// Calls each code hook covering `at` that wants `instruction`'s
    /// opcode, in the order installed; the first that stopped, if any
    /// did.
    fn call_code_hooks(&mut self, at: Address, instruction: &Instruction) -> Option<HookId> {
        let mut stopped = None;
        for hook in self
            .hooks
            .code
            .iter_mut()
            .filter(|hook| hook.range.contains(&at) && hook.hook.wants(instruction.opcode))
        {
            if (hook.hook.callback)(instruction).is_break() {
                stopped.get_or_insert(hook.id);
            }
        }
        stopped
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
                reason: Invalid::Protected,
                ..self.invalid_access(address, size, function_code, cycle, None)
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
                reason: Invalid::Protected,
                ..self.invalid_access(
                    address,
                    size,
                    function_code,
                    BusCycleKind::Write,
                    Some(value),
                )
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
        bit: watch::Bits,
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
            instruction: self.cpu.instructions,
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

    /// The pre-write hooks for an operand write of `value`, `size` of
    /// it, at `address` in `function_code`'s space, aligned, that the
    /// core's memory stores all of and its pages' permissions allow:
    /// each covering a byte of it, in the order installed, given the
    /// write, the memory and a copy of its bytes.  What to store, or
    /// [`PreWrite::Dropped`] with the store's stall charged; [`None`]
    /// if no hook was called.
    #[inline(never)]
    pub(super) fn pre_write(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        value: u32,
    ) -> Option<PreWrite> {
        if !self.pre_hooked(
            address,
            size,
            function_code,
            BusCycleKind::Write,
            watch::PRE_WRITE,
        ) {
            return None;
        }
        let memory = &self.cpu.memory;
        let length = size.bytes();
        let at = |offset: u32| memory.mask(address.offset(offset));
        let stall = memory.write_stall(Space::of(function_code), at(0), size)?;
        let mut copy = value.to_be_bytes();
        let bytes = &mut copy[(4 - length) as usize..];
        let template = PendingWrite::new(
            at(0),
            size,
            function_code,
            self.cpu.instruction_pc,
            self.cpu.instructions,
            self.cpu.stacking,
        );
        let mut dropped = false;
        let mut called = false;
        let mut stopped = None;
        for hook in self
            .cpu
            .hooks
            .pre_write
            .iter_mut()
            .filter(|hook| (0..length).any(|offset| hook.range.contains(&at(offset))))
        {
            called = true;
            // each hook gets the write afresh, but for the drop
            let mut write = template;
            write.set_dropped(dropped);
            if (hook.hook)(&mut write, memory, bytes).is_break() {
                stopped.get_or_insert(hook.id);
            }
            dropped = write.dropped();
        }
        if let Some(id) = stopped {
            self.cpu.stop_request.get_or_insert(Stop::Hook(id));
        }
        if !called {
            return None;
        }
        if dropped {
            self.cpu.stall += stall;
            return Some(PreWrite::Dropped);
        }
        Some(PreWrite::Store(
            bytes
                .iter()
                .fold(0, |value, &byte| value << 8 | u32::from(byte)),
        ))
    }

    /// The read or write coverages and hooks, `bit`, for a cycle that
    /// ended well, on a page that has them.
    fn watch(&mut self, bit: watch::Bits, access: MemoryAccess) {
        if self.cpu.long_operand {
            return;
        }
        self.cover(bit, &access);
        if self.cpu.memory.watched(access.address) & bit == 0 {
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

    /// Records `access`, a cycle the read or write hooks, `bit`, would
    /// see, in the read or write coverages covering a byte of it.
    fn cover(&mut self, bit: watch::Bits, access: &MemoryAccess) {
        let (coverage, kind) = if bit == watch::READ {
            (watch::READ_COVERAGE, CoverageKind::Read)
        } else {
            (watch::WRITE_COVERAGE, CoverageKind::Write)
        };
        let memory = &self.cpu.memory;
        let last = memory.mask(access.address.offset(access.size.bytes() - 1));
        if (memory.watched(access.address) | memory.watched(last)) & coverage == 0 {
            return;
        }
        let mask = memory.address_mask;
        for (_, _, covering) in &mut self.cpu.hooks.coverage {
            if covering.kind() == kind {
                covering.record_bytes(access.address, access.size, mask);
            }
        }
    }

    /// The read or write hooks, `bit`, for a long operand seen once
    /// ([`Cpu::set_long_bus`]), a write's `previous` value as
    /// [`previous`](Self::previous) found it.
    pub(super) fn watch_long(
        &mut self,
        bit: watch::Bits,
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
            instruction: self.cpu.instructions,
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
            instruction: self.cpu.instructions,
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
