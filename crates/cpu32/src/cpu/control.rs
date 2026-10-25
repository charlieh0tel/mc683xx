//! A CPU another layer runs, as its host may change it.
//!
//! A chip keeps its core's memory map in step with its own decode: a
//! host that remapped the core's memory, or ran the core itself, would
//! leave the two apart.  [`CpuControl`] lends what a host may change
//! without that: the registers, the hooks and the pages' permissions,
//! which outlive the chip's remaps; the rest of the CPU it shows
//! read-only, through `Deref`.

use std::ops::Deref;
use std::ops::RangeInclusive;

use super::Cpu;
use super::hooks::Control;
use super::hooks::ExceptionEntry;
use super::hooks::HookId;
use super::hooks::Instruction;
use super::hooks::InvalidAccess;
use super::hooks::MemoryAccess;
use crate::address::Address;
use crate::memory::MemoryError;
use crate::memory::Permissions;
use crate::memory::Space;
use crate::registers::Registers;

/// A CPU lent by what runs it (a chip, a board, the gdb stub's target):
/// its registers, hooks and page permissions to change, the rest to
/// read.
#[derive(Debug)]
pub struct CpuControl<'a> {
    /// The CPU.
    cpu: &'a mut Cpu,
}

impl<'a> CpuControl<'a> {
    /// `cpu`'s controls, for what owns it to lend.
    pub const fn new(cpu: &'a mut Cpu) -> Self {
        Self { cpu }
    }

    /// The registers, to change.
    pub const fn registers_mut(&mut self) -> &mut Registers {
        &mut self.cpu.registers
    }

    /// [`Cpu::add_code_hook`].
    pub fn add_code_hook(
        &mut self,
        range: RangeInclusive<Address>,
        hook: impl FnMut(&Instruction) -> Control + 'static,
    ) -> HookId {
        self.cpu.add_code_hook(range, hook)
    }

    /// [`Cpu::add_block_hook`].
    pub fn add_block_hook(
        &mut self,
        range: RangeInclusive<Address>,
        hook: impl FnMut(&Instruction) -> Control + 'static,
    ) -> HookId {
        self.cpu.add_block_hook(range, hook)
    }

    /// [`Cpu::add_read_hook`].
    pub fn add_read_hook(
        &mut self,
        range: RangeInclusive<Address>,
        hook: impl FnMut(&MemoryAccess) -> Control + 'static,
    ) -> HookId {
        self.cpu.add_read_hook(range, hook)
    }

    /// [`Cpu::add_write_hook`].
    pub fn add_write_hook(
        &mut self,
        range: RangeInclusive<Address>,
        hook: impl FnMut(&MemoryAccess) -> Control + 'static,
    ) -> HookId {
        self.cpu.add_write_hook(range, hook)
    }

    /// [`Cpu::add_invalid_hook`].
    pub fn add_invalid_hook(
        &mut self,
        range: RangeInclusive<Address>,
        hook: impl FnMut(&InvalidAccess) -> Control + 'static,
    ) -> HookId {
        self.cpu.add_invalid_hook(range, hook)
    }

    /// [`Cpu::add_exception_hook`].
    pub fn add_exception_hook(
        &mut self,
        hook: impl FnMut(&ExceptionEntry) -> Control + 'static,
    ) -> HookId {
        self.cpu.add_exception_hook(hook)
    }

    /// [`Cpu::remove_hook`].
    pub fn remove_hook(&mut self, id: HookId) -> bool {
        self.cpu.remove_hook(id)
    }

    /// The permissions of `length` bytes from `start` in `space`, as
    /// [`Memory::protect`](crate::memory::Memory::protect) sets them.
    ///
    /// # Errors
    ///
    /// A range not of whole pages, or past the address lines.
    pub fn protect(
        &mut self,
        space: Space,
        start: Address,
        length: u32,
        permissions: Permissions,
    ) -> Result<(), MemoryError> {
        self.cpu.memory.protect(space, start, length, permissions)
    }
}

impl Deref for CpuControl<'_> {
    type Target = Cpu;

    fn deref(&self) -> &Cpu {
        self.cpu
    }
}

#[cfg(test)]
mod tests {
    use std::ops::ControlFlow;

    use super::CpuControl;
    use crate::address::Address;
    use crate::cpu::Cpu;
    use crate::memory::Memory;
    use crate::memory::PAGE_BYTES;
    use crate::memory::Permissions;
    use crate::memory::Space;

    #[test]
    fn it_changes_registers_hooks_and_permissions_and_shows_the_rest() {
        let mut cpu = Cpu::new(Memory::new(24).unwrap());
        let mut control = CpuControl::new(&mut cpu);
        control.registers_mut().data[0] = 7;
        let id = control.add_code_hook(Address(0)..=Address(0), |_| ControlFlow::Break(()));
        control
            .protect(Space::User, Address(0), PAGE_BYTES, Permissions::READ)
            .unwrap();
        assert_eq!(control.registers.data[0], 7);
        assert!(control.has_hook(id));
        assert!(control.remove_hook(id));
        assert_eq!(
            cpu.memory.permissions(Space::User, Address(0)),
            Permissions::READ
        );
    }
}
