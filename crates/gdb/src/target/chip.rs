//! The chip as the stub's target.

use std::convert::Infallible;

use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::cpu::control::CpuControl;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::size::Size;
use mc683xx_mc68331::chip::Mc68331;

use super::Target;
use super::TargetRun;
use super::Unwritable;
use super::peek_bytes;

/// The supervisor data function code the stub's peeks use.
const DEBUGGER: FunctionCode = FunctionCode::SUPERVISOR_DATA;

impl Target for Mc68331 {
    type Error = Infallible;

    fn with_cpu<R>(&mut self, call: impl FnOnce(&mut CpuControl<'_>) -> R) -> R {
        call(&mut self.cpu_control())
    }

    fn peek(&mut self, address: Address, data: &mut [u8]) -> usize {
        peek_bytes(data, |at| {
            Self::peek(self, address.offset(at), Size::Byte, DEBUGGER)
                .ok()
                .map(|byte| byte as u8)
        })
    }

    fn poke(&mut self, address: Address, data: &[u8]) -> Result<(), Unwritable> {
        Self::poke(self, address, data).map_err(|unserved| Unwritable(unserved.0))
    }

    fn run_for(
        &mut self,
        cycles: Cycles,
        _hit: &dyn Fn() -> bool,
    ) -> Result<TargetRun, Infallible> {
        let target = self.now() + cycles;
        Ok(self.run_until(target).stop.into())
    }

    fn reset(&mut self) -> bool {
        Self::reset(self);
        true
    }
}
