//! The chip on a board wired as the Z3805A is: 256 Kbytes of boot
//! flash on CSBOOT, 256 Kbytes of RAM, an 8-bit peripheral with 12 wait
//! states on CS5, a QSPI peripheral that echoes.  Its boot code
//! programs the chip selects with the Z3805A's values (as `mc683xx-mc68331`'s
//! tests do), stops the watchdog, starts the periodic timer or a QSPI
//! queue in wraparound if the load wants one, then runs a kernel's sled
//! from flash.

use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::cpu::Halt;
use mc683xx_cpu32::cpu::state::Stateless;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::peripheral::BusCycle;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_cpu32::peripheral::PeripheralId;
use mc683xx_cpu32::registers::status;
use mc683xx_mc68331::chip::Builder;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::chip_select::Number;
use mc683xx_modules::peripheral::QspiTransfer;
use mc683xx_modules::peripheral::SpiPeripheral;
use mc683xx_modules::peripheral::Timed;

use crate::kernel::Calibration;
use crate::kernel::Kernel;
use crate::kernel::MOVE_WORD_TO_ABSOLUTE;
use crate::kernel::NOP;
use crate::kernel::RTE;
use crate::kernel::Skip;
use crate::kernel::sled;

/// The SIM's registers with MM set (MC68331UM Table D-3).
const SIM: u32 = 0xff_fa00;
/// SYPCR's word: zero stops the watchdog (MC68331UM 4.2.10).
const SYPCR: u32 = SIM + 0x20;
/// PICR (MC68331UM D.2.12).
const PICR: u32 = SIM + 0x22;
/// PITR (MC68331UM D.2.13).
const PITR: u32 = SIM + 0x24;
/// PICR: level 6, vector $50.
const PIT_LEVEL_VECTOR: u16 = 0x0650;
/// The PIT's vector.
const PIT_VECTOR: u32 = 0x50;
/// PITR: PITM 1, the shortest period, 4 EXTAL periods (1024 clocks at
/// the reset SYNCR).
const PIT_SHORTEST: u16 = 0x0001;
/// The QSM's registers with MM set (MC68331UM Table D-14).
const QSM: u32 = 0xff_fc00;
/// PQSPAR:DDRQS (MC68331UM D.4.9).
const PQSPAR_DDRQS: u32 = QSM + 0x16;
/// SPCR0 (MC68331UM D.4.10).
const SPCR0: u32 = QSM + 0x18;
/// SPCR1 (MC68331UM D.4.11).
const SPCR1: u32 = QSM + 0x1a;
/// SPCR2 (MC68331UM D.4.12).
const SPCR2: u32 = QSM + 0x1c;
/// The QSPI's transmit RAM, a word an entry (MC68331UM D.4.16).
const TRANSMIT_RAM: u32 = QSM + 0x120;
/// The QSPI's command RAM, a byte an entry.
const COMMAND_RAM: u32 = QSM + 0x140;
/// Entries in the queue: all 16.
const QUEUE_ENTRIES: u32 = 16;
/// PQSPAR:DDRQS: PCS[3:0], MOSI and MISO the QSPI's; PCS[3:0], SCK and
/// MOSI outputs (MC68331UM D.4.9).
const QSPI_PINS: u16 = 0x7b7e;
/// SPCR0: master, 16 bits, CPOL, SPBR 2, the fastest SCK, as the
/// 58503A's FPGA block transfers set it (MC68331UM D.4.10).
const SPCR0_FASTEST: u16 = 0x8202;
/// SPCR2: WREN, ENDQP $F, NEWQP $0, no SPIF interrupt: the queue wraps
/// for ever (MC68331UM D.4.12).
const SPCR2_WRAP: u16 = 0x4f00;
/// SPCR1: SPE, DSCKL 1, DTL 1, as the 58503A sets it (MC68331UM
/// D.4.11); the last write, which starts the queue.
const SPCR1_START: u16 = 0x8101;
/// A command byte: CONT, BITSE (SPCR0's 16 bits), PCS 0001 (MC68331UM
/// D.4.16).
const COMMAND_WORDS: u8 = 0xc1;
/// A command byte without BITSE: 8 bits.
const COMMAND_BYTES: u8 = 0x81;
/// The transmit RAM's pattern, entry `n` this exclusive-or `n`.
const TRANSMIT_PATTERN: u16 = 0xa5c3;
/// Bytes in a vector (CPU32RM 6.1.4).
const VECTOR_BYTES: u32 = 4;
/// The Z3805A's chip-select registers and values, as its boot code
/// writes them: offset from `SIM`, value.
const Z3805A_SELECTS: [(u32, u16); 26] = [
    (0x44, 0x2bff),
    (0x46, 0x03af),
    (0x48, 0x0005),
    (0x4a, 0x6870),
    (0x4c, 0x1003),
    (0x4e, 0x6830),
    (0x50, 0x0405),
    (0x52, 0x6870),
    (0x54, 0x1003),
    (0x56, 0x5030),
    (0x58, 0x1003),
    (0x5a, 0x3030),
    (0x5c, 0x5000),
    (0x5e, 0x7830),
    (0x60, 0x3003),
    (0x62, 0x7b30),
    (0x64, 0x0005),
    (0x66, 0x7070),
    (0x68, 0x0405),
    (0x6a, 0x7070),
    (0x6c, 0x2000),
    (0x6e, 0x3bf0),
    (0x70, 0x4001),
    (0x72, 0x78f0),
    (0x74, 0xfff8),
    (0x76, 0x2bc4),
];
/// The boot flash.
const FLASH_BYTES: usize = 256 << 10;
/// The RAM.
const RAM_BYTES: usize = 256 << 10;
/// The supervisor stack, in RAM.
const STACK: u32 = 0x10_fffe;
/// Where the boot code starts, past the vectors.
const BOOT: u32 = 0x400;
/// The PIT's handler: RTE.
const HANDLER: u32 = 0x3_fffe;
/// RAM on CS0, CS2 and CS3, as the Z3805A's selects place it.
pub const RAM: u32 = 0x10_0000;
/// The 8-bit, 12-wait-state peripheral on CS5.
pub const PERIPHERAL: u32 = 0x30_0000;
/// Clocks one iteration runs: well inside the sled for every kernel.
pub const BUDGET: Cycles = Cycles(20_000);

/// The board's kernels: code from flash, data from RAM through the
/// page tables or from CS5's peripheral through the bus.
pub const KERNELS: &[Kernel] = &[
    Kernel {
        name: "flash_nop",
        code: &[NOP],
    },
    Kernel {
        // MOVE.W ($100100).L,D0
        name: "ram_move",
        code: &[0x3039, 0x0010, 0x0100],
    },
    Kernel {
        // MOVE.B ($300000).L,D0
        name: "peripheral_move",
        code: &[0x1039, 0x0030, 0x0000],
    },
];

/// What runs beside the sled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Load {
    /// Nothing: one quantum per run.
    Idle,
    /// The periodic timer at its shortest period, interrupts masked: a
    /// sync every period.
    PitMasked,
    /// The periodic timer at its shortest period, each period an
    /// interrupt whose handler is RTE.
    PitInterrupting,
    /// A 16-entry QSPI queue in wraparound at the fastest SCK, 16-bit
    /// transfers: 83 clocks an entry, a sync at each transfer, where
    /// the peripheral is called.
    QspiWords,
    /// The same queue with 8-bit transfers: 51 clocks an entry.
    QspiBytes,
    /// The 8-bit queue to a peripheral whose calls may be deferred: no
    /// sync for it.
    QspiBytesDeferred,
}

impl Load {
    /// Whether the periodic timer runs.
    const fn timer(self) -> bool {
        matches!(self, Self::PitMasked | Self::PitInterrupting)
    }

    /// The QSPI queue's command byte, if a queue runs.
    const fn command(self) -> Option<u8> {
        match self {
            Self::QspiWords => Some(COMMAND_WORDS),
            Self::QspiBytes | Self::QspiBytesDeferred => Some(COMMAND_BYTES),
            Self::Idle | Self::PitMasked | Self::PitInterrupting => None,
        }
    }
}

/// A peripheral that answers every read with one value.
#[derive(Debug)]
struct Latch(u32);

impl MappedPeripheral for Latch {
    fn read(&mut self, cycle: BusCycle) -> Result<u32, BusError> {
        Ok(self.0 & cycle.size.mask())
    }

    fn write(&mut self, _cycle: BusCycle, _value: u32) -> Result<(), BusError> {
        Ok(())
    }
}

impl Timed for Latch {}

impl Stateless for Latch {}

/// A QSPI peripheral that shifts back what it is sent.
#[derive(Debug, Default)]
struct Echo {
    /// Transfers so far.
    transfers: u64,
    /// What `deferrable` returns.
    deferrable: bool,
}

// Its count is the benchmark's, not state.
impl Stateless for Echo {}

impl SpiPeripheral for Echo {
    fn transfer(&mut self, transfer: QspiTransfer) -> u16 {
        self.transfers += 1;
        transfer.out
    }

    fn deferrable(&self) -> bool {
        self.deferrable
    }
}

/// A model on no pin that counts the quanta the chip brings it to.
#[derive(Debug, Default)]
pub struct QuantumCounter {
    /// Quanta so far.
    pub quanta: u64,
}

impl MappedPeripheral for QuantumCounter {
    fn read(&mut self, _cycle: BusCycle) -> Result<u32, BusError> {
        Err(BusError)
    }

    fn write(&mut self, _cycle: BusCycle, _value: u32) -> Result<(), BusError> {
        Err(BusError)
    }
}

impl Timed for QuantumCounter {
    fn advance(&mut self, _now: Cycles) {
        self.quanta += 1;
    }
}

mc683xx_cpu32::unsavable!(QuantumCounter, "a quantum counter");

/// The chip, booted to its sled.
#[derive(Debug)]
pub struct Board {
    /// The chip.
    pub chip: Mc68331,
    /// The quantum counter's id.
    pub quanta: PeripheralId,
    /// Where the sled starts.
    sled: u32,
    /// What runs beside the sled.
    load: Load,
}

/// CS`number`.
fn cs(number: u8) -> ChipSelect {
    ChipSelect::Numbered(Number::new(number).expect("a chip select"))
}

/// MOVE.W #`value`,(`address`).L.
fn store_word(address: u32, value: u16) -> [u16; 4] {
    [
        MOVE_WORD_TO_ABSOLUTE,
        value,
        (address >> 16) as u16,
        address as u16,
    ]
}

/// Writes `words` at `at` in `image`, big-endian.
fn put(image: &mut [u8], at: u32, words: &[u16]) {
    for (index, word) in words.iter().enumerate() {
        let at = at as usize + 2 * index;
        image[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
}

impl Board {
    /// The board, booted through its chip-select setup to `kernel`'s
    /// sled.
    ///
    /// # Panics
    ///
    /// If the boot code does not reach the sled.
    #[must_use]
    pub fn new(kernel: &Kernel, load: Load) -> Self {
        let mut boot: Vec<u16> = Z3805A_SELECTS
            .iter()
            .flat_map(|&(offset, value)| store_word(SIM + offset, value))
            .chain(store_word(SYPCR, 0))
            .collect();
        if load.timer() {
            boot.extend(store_word(PICR, PIT_LEVEL_VECTOR));
            boot.extend(store_word(PITR, PIT_SHORTEST));
        }
        if let Some(command) = load.command() {
            boot.extend(store_word(PQSPAR_DDRQS, QSPI_PINS));
            for entry in 0..QUEUE_ENTRIES {
                boot.extend(store_word(
                    TRANSMIT_RAM + 2 * entry,
                    TRANSMIT_PATTERN ^ entry as u16,
                ));
            }
            for pair in (0..QUEUE_ENTRIES).step_by(2) {
                boot.extend(store_word(
                    COMMAND_RAM + pair,
                    u16::from_be_bytes([command, command]),
                ));
            }
            boot.extend(store_word(SPCR0, SPCR0_FASTEST));
            boot.extend(store_word(SPCR2, SPCR2_WRAP));
            boot.extend(store_word(SPCR1, SPCR1_START));
        }
        let sled_start = BOOT + 2 * boot.len() as u32;
        let mut flash = vec![0; FLASH_BYTES];
        flash[sled_start as usize..HANDLER as usize]
            .copy_from_slice(&sled(kernel, (HANDLER - sled_start) as usize));
        flash[0..4].copy_from_slice(&STACK.to_be_bytes());
        flash[4..8].copy_from_slice(&BOOT.to_be_bytes());
        let vector = (PIT_VECTOR * VECTOR_BYTES) as usize;
        flash[vector..vector + 4].copy_from_slice(&HANDLER.to_be_bytes());
        put(&mut flash, BOOT, &boot);
        put(&mut flash, HANDLER, &[RTE]);

        let mut builder = Builder::default();
        builder
            .memory(flash, false, &[ChipSelect::Boot])
            .expect("CSBOOT is free");
        builder
            .memory(vec![0; RAM_BYTES], true, &[cs(0), cs(2), cs(3)])
            .expect("RAM's selects are free");
        builder
            .peripheral(Latch(0x5a5a), &[cs(5)])
            .expect("CS5 is free");
        builder.qspi(Echo {
            transfers: 0,
            deferrable: load == Load::QspiBytesDeferred,
        });
        let quanta = builder.unwired_peripheral(QuantumCounter::default());
        let mut chip = builder.build();
        while chip.cpu().registers.pc != sled_start {
            chip.run_for(Cycles(1));
            assert_eq!(chip.halted(), None::<Halt>, "the boot code halted");
        }
        let mut board = Self {
            chip,
            quanta,
            sled: sled_start,
            load,
        };
        board.prepare();
        board
    }

    /// Back to the sled's start with an empty stack, interrupts masked
    /// or not as the load wants.
    pub fn prepare(&mut self) {
        let mask = if self.load == Load::PitInterrupting {
            0
        } else {
            status::INTERRUPT_MASK
        };
        let registers = self.chip.registers_mut();
        registers.set_sr(status::S | mask);
        registers.ssp = STACK;
        registers.pc = self.sled;
    }

    /// One iteration: the sled from its start for `budget` clocks.
    pub fn iteration(&mut self, budget: Cycles) -> Cycles {
        self.prepare();
        self.chip.run_for(budget).cycles
    }

    /// QSPI transfers so far.
    #[must_use]
    pub fn transfers(&self) -> u64 {
        self.chip
            .qspi_peripheral::<Echo>()
            .expect("the QSPI peripheral")
            .transfers
    }

    /// Quanta so far.
    #[must_use]
    pub fn quanta(&self) -> u64 {
        self.chip
            .peripheral::<QuantumCounter>(self.quanta)
            .expect("the quantum counter")
            .quanta
    }

    /// What an iteration of `kernel` does with `load`, or why it
    /// cannot run.
    ///
    /// # Panics
    ///
    /// If it takes an exception other than the PIT's, or runs off its
    /// sled.
    pub fn calibrate(kernel: &Kernel, load: Load, budget: Cycles) -> Result<Calibration, Skip> {
        let mut board = Self::new(kernel, load);
        let calibration = Calibration::measure(budget, || {
            let run = board.chip.run_for(Cycles(1));
            let taken = board.chip.cpu().exception_taken();
            assert!(
                taken.is_none_or(|exception| crate::kernel::unimplemented(exception)
                    || load == Load::PitInterrupting
                        && u32::from(exception.vector().0) == PIT_VECTOR),
                "{}: {taken:?}",
                kernel.name
            );
            (run.cycles, taken)
        })?;
        assert!(
            board.chip.cpu().registers.pc < HANDLER + 2,
            "{}: ran off its sled",
            kernel.name
        );
        Ok(calibration)
    }
}
