//! The chip as the stub's target.

use std::convert::Infallible;

use mc683xx_cpu32::CpuControl;
use mc683xx_cpu32::Cycles;
use mc683xx_mc68331::Mc68331;
use mc683xx_target::Address;
use mc683xx_target::FunctionCode;
use mc683xx_target::Size;

use super::AccessError;
use super::Target;
use super::TargetReset;
use super::TargetRun;

/// The chip reads in the space the function code names, through its
/// decode, a peripheral's bytes by its peek; it writes in supervisor
/// space alone, into memory, and a peripheral's bytes by its poke: a
/// call to the peripheral for each run of its bytes.
impl Target for Mc68331 {
    type Error = Infallible;

    fn cpu_scope<R>(&mut self, call: impl FnOnce(&mut CpuControl<'_>) -> R) -> Option<R> {
        Some(call(&mut self.cpu_control()))
    }

    fn read_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        data: &mut [u8],
    ) -> Result<usize, AccessError<Infallible>> {
        if self.peek_bytes(address, function_code, data).is_ok() {
            return Ok(data.len());
        }
        // the bytes before the first that fails, a byte at a time
        Ok((0..)
            .zip(data.iter_mut())
            .find_map(|(offset, slot)| {
                match self.peek(address.offset(offset), Size::Byte, function_code) {
                    Ok(byte) => {
                        *slot = byte as u8;
                        None
                    }
                    Err(_) => Some(offset as usize),
                }
            })
            .unwrap_or(data.len()))
    }

    fn write_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        data: &[u8],
    ) -> Result<(), AccessError<Infallible>> {
        if ![
            FunctionCode::SupervisorData,
            FunctionCode::SupervisorProgram,
        ]
        .contains(&function_code)
        {
            return Err(AccessError::Unserved(address));
        }
        if Self::write(self, address, data).is_ok() {
            return Ok(());
        }
        // a peripheral's bytes among them: poked, a run at a time
        self.poke_bytes(address, function_code, data)
            .map_err(|_| AccessError::Unserved(address))
    }

    fn run_for(
        &mut self,
        budget: Cycles,
        _hit: &dyn Fn() -> bool,
    ) -> Result<TargetRun, Infallible> {
        Ok(self.run_until(self.now() + budget).stop.into())
    }

    fn reset(&mut self) -> Result<TargetReset, Infallible> {
        Self::reset(self);
        Ok(TargetReset::Done)
    }
}
