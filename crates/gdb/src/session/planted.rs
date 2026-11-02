//! A target without the CPU's controls (one under BDM control): its
//! software breakpoints are BGND written over the instruction (CPU32RM
//! 4.4, BGND), so they need memory the target can write; one in ROM is
//! refused.  BGND enters background mode with PCC at the breakpoint and
//! RPC past it; the stub puts RPC back on the breakpoint, so the
//! instruction runs once gdb has removed it.  A stop is matched to a
//! breakpoint by its address alone: gdb plants in a data space while
//! the CPU executes in program space, and boards alias the two, so
//! distinct memory at one address in different spaces is not
//! supported.  Each breakpoint is removed from the address space it
//! was planted in, and only if BGND is still there.  Memory gdb reads
//! or writes over one is the word it replaced, unless the program has
//! written over BGND: then the word is the program's, and the
//! breakpoint is retired.
//!
//! A write that fails or does not read back, planting BGND or putting
//! the word back, may have written part of the word: the breakpoint is
//! then uncertain, its word BGND, the original or a mix of their bytes,
//! and is put back from any of these.

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
    /// Whether BGND read back after it was planted and no write there
    /// has failed since: if not, the word may be BGND, the original or
    /// any mix of their bytes.
    pub(super) verified: bool,
}

impl Breakpoint {
    /// Whether `word`, read at the breakpoint, needs nothing put back:
    /// anything but BGND for a verified breakpoint; for an uncertain
    /// one, the original, or a word not made of BGND's and the
    /// original's bytes, the program's.
    fn settled(&self, word: [u8; 2]) -> bool {
        if self.verified {
            return word != BGND;
        }
        word == self.original
            || (0..BGND.len())
                .any(|byte| word[byte] != BGND[byte] && word[byte] != self.original[byte])
    }
}

impl<T: Target> Session<'_, T> {
    /// Plants BGND at `address`, an even one, in the session's address
    /// space: false if the word there cannot be read, or does not read
    /// back as BGND, as in ROM.  A breakpoint whose BGND was written
    /// over since is planted again; an uncertain one is planted again
    /// over the original it keeps.  If the write may have changed the
    /// word and it cannot be put back, the breakpoint is kept,
    /// uncertain, for the session's end to try again.
    pub(super) fn plant(&mut self, address: u32) -> Result<bool, AccessError<T::Error>> {
        let space = self.function_code;
        if let Some(index) = self.planted_at(address, space) {
            let planted = self.planted[index];
            if self.settled(planted)? {
                self.planted.remove(index);
            } else if planted.verified {
                return Ok(true);
            }
        }
        if !address.is_multiple_of(2) {
            return Ok(false);
        }
        let index = match self.planted_at(address, space) {
            Some(index) => index,
            None => {
                let Ok(original) = self.word(address, space) else {
                    return Ok(false);
                };
                self.planted.push(Breakpoint {
                    address,
                    function_code: space,
                    original,
                    verified: false,
                });
                self.planted.len() - 1
            }
        };
        let written = self
            .target
            .write_memory(Address(address), space, &BGND)
            .and_then(|()| self.word(address, space));
        if written.is_ok_and(|word| word == BGND) {
            self.planted[index].verified = true;
            return Ok(true);
        }
        if self.put_back(index).unwrap_or(false) {
            self.planted.remove(index);
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
            if self.planted[index].address == address {
                if self.put_back(index)? {
                    self.planted.remove(index);
                    continue;
                }
                all = false;
            }
            index += 1;
        }
        Ok(all)
    }

    /// Puts back the word the breakpoint at `index` replaced unless the
    /// word there is settled (a word the program or gdb wrote over it
    /// since stands): whether it is settled now.  A write tried leaves
    /// the breakpoint uncertain until it comes out.
    fn put_back(&mut self, index: usize) -> Result<bool, AccessError<T::Error>> {
        let planted = self.planted[index];
        if self.settled(planted)? {
            return Ok(true);
        }
        self.planted[index].verified = false;
        let space = planted.function_code;
        self.target
            .write_memory(Address(planted.address), space, &planted.original)?;
        Ok(self.word(planted.address, space)? == planted.original)
    }

    /// Whether `planted`'s word, in its address space, needs nothing
    /// put back ([`Breakpoint::settled`]).
    fn settled(&mut self, planted: Breakpoint) -> Result<bool, AccessError<T::Error>> {
        Ok(planted.settled(self.word(planted.address, planted.function_code)?))
    }

    /// Puts back every breakpoint planted, each tried whatever the last
    /// did: those that did not come out, their word not read back or
    /// not read at all, are kept.  The first of the target's errors.
    pub(super) fn unplant_all(&mut self) -> Result<(), T::Error> {
        let mut first = Ok(());
        let mut index = 0;
        while index < self.planted.len() {
            match self.put_back(index) {
                Ok(true) => {
                    self.planted.remove(index);
                    continue;
                }
                Err(AccessError::Target(error)) if first.is_ok() => first = Err(error),
                Ok(false) | Err(_) => {}
            }
            index += 1;
        }
        first
    }

    /// Retires the breakpoints planted in `space` that `length` bytes at
    /// `address` reach whose word is settled: the program wrote over
    /// it, so the word there is the program's, neither shadowed nor put
    /// back, or an uncertain one's original is there again.
    pub(super) fn retire_overwritten(
        &mut self,
        address: u32,
        length: usize,
        space: FunctionCode,
    ) -> Result<(), AccessError<T::Error>> {
        let mut index = 0;
        while index < self.planted.len() {
            let planted = self.planted[index];
            let reached = planted.function_code == space
                && (0..BGND.len()).any(|byte| offset(planted, byte, address) < length);
            if reached && self.settled(planted)? {
                self.planted.remove(index);
                continue;
            }
            index += 1;
        }
        Ok(())
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
                let offset = offset(*planted, byte, address);
                if offset < length {
                    bytes.push((offset, index, byte));
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

/// Byte `byte` of `planted`'s word, as an offset from `address`: past
/// an access's length if the access does not reach it.
fn offset(planted: Breakpoint, byte: usize, address: u32) -> usize {
    planted
        .address
        .wrapping_add(byte as u32)
        .wrapping_sub(address) as usize
}
