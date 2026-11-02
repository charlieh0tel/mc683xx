//! A board as the stub's target: its chip, run with the board's peripherals
//! and nets.

use mc683xx_board::board::Board;
use mc683xx_board::condition::Conditions;
use mc683xx_board::error::RunError;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cpu::control::CpuControl;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_modules::sim::ResetCause;
use mc683xx_target::address::Address;
use mc683xx_target::function_code::FunctionCode;

use super::AccessError;
use super::Target;
use super::TargetRun;
use super::widen;

impl Target for Board<Mc68331> {
    type Error = RunError;

    fn with_cpu<R>(&mut self, call: impl FnOnce(&mut CpuControl<'_>) -> R) -> Option<R> {
        Some(call(&mut self.chip_mut().cpu_control()))
    }

    fn read_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        data: &mut [u8],
    ) -> Result<usize, AccessError<RunError>> {
        Target::read_memory(self.chip_mut(), address, function_code, data).map_err(widen)
    }

    fn write_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        data: &[u8],
    ) -> Result<(), AccessError<RunError>> {
        Target::write_memory(self.chip_mut(), address, function_code, data).map_err(widen)
    }

    /// Runs the board to the board time `cycles` past the chip's now,
    /// and at least a picosecond, so the board's peripherals and nets keep
    /// time with the chip.
    fn run_for(&mut self, cycles: Cycles, _hit: &dyn Fn() -> bool) -> Result<TargetRun, RunError> {
        let chip = self.chip().now() + cycles;
        let until = self.system_clock().time(chip).max(self.now() + 1);
        let stop = Self::run_until(self, until)?.stop;
        Ok(match stop {
            Stop::Requested if !self.fired().is_empty() => TargetRun::Condition,
            other => other.into(),
        })
    }

    /// An external reset, as the board asserting RESET makes it
    /// ([`Board::reset_for`]): the chip's reset and the RESET it then
    /// drives on the board's nets, now.  Nothing while the board holds
    /// RESET low.
    fn reset(&mut self) -> Result<bool, RunError> {
        self.reset_for(ResetCause::External);
        Ok(true)
    }

    fn conditions(&mut self) -> Option<&mut dyn Conditions> {
        Some(self)
    }
}
