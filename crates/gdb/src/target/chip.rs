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
use super::TargetRun;
use super::peek_bytes;

/// The chip reads in the space the function code names, through its
/// decode; it writes in supervisor space alone, where its pokes go.
impl Target for Mc68331 {
    type Error = Infallible;

    fn with_cpu<R>(&mut self, call: impl FnOnce(&mut CpuControl<'_>) -> R) -> Option<R> {
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
            FunctionCode::SUPERVISOR_DATA,
            FunctionCode::SUPERVISOR_PROGRAM,
        ]
        .contains(&function_code)
        {
            return Err(AccessError::Unserved(address));
        }
        Self::poke(self, address, data).map_err(|unserved| AccessError::Unserved(unserved.0))
    }

    fn run_for(
        &mut self,
        cycles: Cycles,
        _hit: &dyn Fn() -> bool,
    ) -> Result<TargetRun, Infallible> {
        let target = self.now() + cycles;
        Ok(self.run_until(target).stop.into())
    }

    fn reset(&mut self) -> Result<bool, Infallible> {
        Self::reset(self);
        Ok(true)
    }
}
