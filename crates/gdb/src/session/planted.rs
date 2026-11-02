//! A target without the CPU's controls (one under BDM control): its
//! software breakpoints are BGND written over the instruction (CPU32RM
//! 4.4, BGND), so they need memory the target can write; one in ROM is
//! refused.  BGND enters background mode with PCC at the breakpoint and
//! RPC past it; the stub puts RPC back on the breakpoint, so the
//! instruction runs once gdb has removed it.  Each breakpoint is
//! removed from the address space it was planted in, and only if BGND
//! is still there.  Memory gdb reads or writes over one is the word it
//! replaced.

use mc683xx_target::address::Address;
use mc683xx_target::function_code::FunctionCode;

use super::Session;
use crate::target::AccessError;
use crate::target::Target;

/// BGND (CPU32RM 4.4, BGND): a software breakpoint.
pub(super) const BGND: [u8; 2] = [0x4a, 0xfa];

/// A software breakpoint the session planted, or may have.
#[derive(Debug, Clone, Copy)]
pub(super) struct Breakpoint {
    /// Its address.
    pub(super) address: u32,
    /// The address space it was planted in.
    function_code: FunctionCode,
    /// The word BGND replaced.
    pub(super) original: [u8; 2],
}

impl<T: Target> Session<'_, T> {
    /// Plants BGND at `address`, an even one, in the session's address
    /// space: false if the word there cannot be read, or does not read
    /// back as BGND, as in ROM.  A breakpoint whose BGND was written
    /// over since is planted again.  If BGND was written and the word
    /// cannot be put back, the breakpoint is kept for the session's
    /// end to try again.
    pub(super) fn plant(&mut self, address: u32) -> Result<bool, AccessError<T::Error>> {
        let space = self.function_code;
        if let Some(index) = self.planted_at(address, space) {
            if self.word(address, space)? == BGND {
                return Ok(true);
            }
            self.planted.remove(index);
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
        self.planted.push(planted);
        let written = self
            .target
            .write_memory(Address(address), space, &BGND)
            .and_then(|()| self.word(address, space));
        if written.is_ok_and(|word| word == BGND) {
            return Ok(true);
        }
        if self.put_back(planted).unwrap_or(false) {
            self.planted.pop();
        }
        Ok(false)
    }

    /// Puts back the words breakpoints at `address` replaced, in each
    /// one's address space (gdb names none): false if one did not read
    /// back, which is kept for the session's end to try again.
    /// Removing one there is none of is done already.  On an error the
    /// rest are kept.
    pub(super) fn unplant(&mut self, address: u32) -> Result<bool, AccessError<T::Error>> {
        let mut all = true;
        let mut index = 0;
        while index < self.planted.len() {
            let planted = self.planted[index];
            if planted.address == address {
                if self.put_back(planted)? {
                    self.planted.remove(index);
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
    fn put_back(&mut self, planted: Breakpoint) -> Result<bool, AccessError<T::Error>> {
        let space = planted.function_code;
        if self.word(planted.address, space)? == BGND {
            self.target
                .write_memory(Address(planted.address), space, &planted.original)?;
            return Ok(self.word(planted.address, space)? == planted.original);
        }
        Ok(true)
    }

    /// Removes every breakpoint planted; the first error, after trying
    /// them all.
    pub(super) fn unplant_all(&mut self) -> Result<(), AccessError<T::Error>> {
        let mut addresses: Vec<u32> = self.planted.iter().map(|planted| planted.address).collect();
        addresses.sort_unstable();
        addresses.dedup();
        let mut first = Ok(());
        for address in addresses {
            let removed = self.unplant(address).map(|_| ());
            if first.is_ok() {
                first = removed;
            }
        }
        first
    }

    /// The bytes of breakpoints planted in `space` that fall in `length`
    /// bytes at `address`: each byte's offset there, its breakpoint's
    /// index and the byte's index in the word.
    pub(super) fn overlaps(
        &self,
        address: u32,
        length: usize,
        space: FunctionCode,
    ) -> Vec<(usize, usize, usize)> {
        let mut bytes = Vec::new();
        for (index, planted) in self.planted.iter().enumerate() {
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
    fn planted_at(&self, address: u32, space: FunctionCode) -> Option<usize> {
        self.planted
            .iter()
            .position(|planted| planted.address == address && planted.function_code == space)
    }

    /// The word at `address` in `space`.
    fn word(
        &mut self,
        address: u32,
        space: FunctionCode,
    ) -> Result<[u8; 2], AccessError<T::Error>> {
        let mut word = [0; 2];
        let read = self
            .target
            .read_memory(Address(address), space, &mut word)?;
        if read < word.len() {
            return Err(AccessError::Unserved(Address(address).offset(read as u32)));
        }
        Ok(word)
    }
}
