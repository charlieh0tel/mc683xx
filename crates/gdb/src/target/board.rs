//! A board as the stub's target: its chip, run with the board's peripherals
//! and nets.

use mc683xx_board::board::Board;

use mc683xx_board::error::RunError;
use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::cpu::control::CpuControl;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_mc68331::chip::Mc68331;

use super::Target;
use super::TargetRun;
use super::Unwritable;

impl Target for Board<Mc68331> {
    type Error = RunError;

    fn with_cpu<R>(&mut self, call: impl FnOnce(&mut CpuControl<'_>) -> R) -> R {
        call(&mut self.chip_mut().cpu_control())
    }

    fn peek(&mut self, address: Address, data: &mut [u8]) -> usize {
        Target::peek(self.chip_mut(), address, data)
    }

    fn poke(&mut self, address: Address, data: &[u8]) -> Result<(), Unwritable> {
        Target::poke(self.chip_mut(), address, data)
    }

    /// Runs the board to the board time `cycles` past the chip's now,
    /// and at least a picosecond, so the board's peripherals and nets keep
    /// time with the chip.
    fn run_for(&mut self, cycles: Cycles, _hit: &dyn Fn() -> bool) -> Result<TargetRun, RunError> {
        let chip = self.chip().now() + cycles;
        let until = self.system_clock().time(chip).max(self.now() + 1);
        Ok(Self::run_until(self, until)?.stop.into())
    }

    /// An external reset of the chip; the board drives RESET on its
    /// nets as it does for any reset the chip reports.
    fn reset(&mut self) -> bool {
        Target::reset(self.chip_mut())
    }
}
