//! The chip as the stub's target.

use std::convert::Infallible;

use mc683xx_cpu32::CpuControl;
use mc683xx_cpu32::Cycles;
use mc683xx_mc68331::Mc68331;
use mc683xx_target::Address;
use mc683xx_target::FunctionCode;

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
        Ok(match self.peek_bytes(address, function_code, data) {
            Ok(()) => data.len(),
            // filled up to the byte it names, on the address lines
            Err(error) => error.address().map_or(0, |failed| {
                self.cpu()
                    .memory
                    .mask(Address(failed.0.wrapping_sub(address.0)))
                    .0 as usize
            }),
        })
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
        self.poke_bytes(address, function_code, data)
            .map_err(|error| AccessError::Unserved(error.address().unwrap_or(address)))
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
