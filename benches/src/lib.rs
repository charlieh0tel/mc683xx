//! Shared setup for the benchmarks (docs/performance.md): kernels of
//! instructions, a bare core on RAM to run them, and an MC68331 board
//! wired as the Z3805A is.

pub mod bare;
pub mod board;
pub mod kernel;

#[cfg(test)]
mod tests {
    use crate::bare;
    use crate::board::BUDGET;
    use crate::board::Board;
    use crate::board::KERNELS;
    use crate::board::Load;
    use crate::kernel;
    use crate::kernel::Skip;

    #[test]
    fn every_core_kernel_runs_or_is_skipped() {
        for kernel in kernel::CORE {
            match bare::calibrate(kernel, bare::BUDGET) {
                Ok(calibration) => assert!(calibration.instructions > 0, "{}", kernel.name),
                Err(Skip::Unimplemented) => {}
            }
        }
    }

    #[test]
    fn the_board_runs_its_kernels_and_syncs_at_the_load() {
        for kernel in KERNELS {
            assert!(Board::calibrate(kernel, Load::Idle, BUDGET).is_ok());
        }
        for load in [
            Load::Idle,
            Load::PitMasked,
            Load::PitInterrupting,
            Load::QspiWords,
            Load::QspiBytes,
            Load::QspiBytesDeferred,
        ] {
            let mut board = Board::new(&KERNELS[0], load);
            let before = board.quanta();
            board.iteration(BUDGET);
            let quanta = board.quanta() - before;
            if matches!(load, Load::Idle | Load::QspiBytesDeferred) {
                assert_eq!(quanta, 1, "{load:?}");
            } else {
                assert!(quanta > 1, "{load:?}");
            }
        }
    }

    #[test]
    fn the_qspi_queue_wraps_through_the_sled() {
        for load in [Load::QspiWords, Load::QspiBytes, Load::QspiBytesDeferred] {
            let mut board = Board::new(&KERNELS[0], load);
            let before = board.transfers();
            board.iteration(BUDGET);
            // 83 or 51 clocks an entry
            assert!(board.transfers() - before > 200, "{load:?}");
        }
    }
}
