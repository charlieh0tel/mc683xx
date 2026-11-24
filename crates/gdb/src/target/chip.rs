//! The chip as the stub's target.

use std::convert::Infallible;

use mc683xx_cpu32::cpu::control::CpuControl;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_target::address::Address;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::size::Size;

use super::AccessError;
use super::Target;
use super::TargetReset;
use super::TargetRun;
use super::peek_bytes;

/// The chip reads in the space the function code names, through its
/// decode, a peripheral's bytes by its peek; it writes in supervisor
/// space alone, into memory, and a peripheral's bytes by its poke.
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
        Ok(peek_bytes(data, |at| {
            Self::peek(self, address.offset(at), Size::Byte, function_code)
                .ok()
                .map(|byte| byte as u8)
        }))
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
        // a peripheral's bytes among them: poked, a byte at a time
        data.iter().zip(0..).try_for_each(|(&byte, offset)| {
            let at = address.offset(offset);
            Self::poke(self, at, Size::Byte, function_code, u32::from(byte))
                .map_err(|_| AccessError::Unserved(at))
        })
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
