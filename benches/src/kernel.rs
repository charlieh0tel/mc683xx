//! Kernels: a few instructions, repeated end to end to fill a sled, so
//! a run is the kernel and nothing else.  A kernel whose instructions
//! the core does not have yet is skipped (`Calibration::measure`), so
//! the list can name instructions before they land.

use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::exception::Exception;
use mc683xx_cpu32::exception::Vector;

/// NOP (CPU32RM 4, NOP).
pub const NOP: u16 = 0x4e71;
/// RTE (CPU32RM 4, RTE).
pub const RTE: u16 = 0x4e73;
/// MOVE.W #imm,(xxx).L (CPU32RM 4, MOVE).
pub const MOVE_WORD_TO_ABSOLUTE: u16 = 0x33fc;

/// A kernel: its name and its code, which must run straight through
/// or loop on itself; it is repeated to fill the sled.
#[derive(Debug, Clone, Copy)]
pub struct Kernel {
    /// The benchmark's name.
    pub name: &'static str,
    /// The instruction words.
    pub code: &'static [u16],
}

/// The bare core's kernels.  Registers start as `bare::prepare` sets
/// them: A0 and A1 at two data buffers, D7 $FFFF, the rest zero.
pub const CORE: &[Kernel] = &[
    Kernel {
        name: "nop",
        code: &[NOP],
    },
    Kernel {
        // MOVEQ #1,D0
        name: "moveq",
        code: &[0x7001],
    },
    Kernel {
        // MOVE.L D0,D1
        name: "move_register",
        code: &[0x2200],
    },
    Kernel {
        // MOVE.W (A0),D1; MOVE.W D1,(A1)
        name: "move_indirect",
        code: &[0x3210, 0x3281],
    },
    Kernel {
        // MOVE.L (A0)+,(A1)+
        name: "move_postincrement",
        code: &[0x22d8],
    },
    Kernel {
        // MOVE.W (16,A0),D2
        name: "move_displacement",
        code: &[0x3428, 0x0010],
    },
    Kernel {
        // MOVE.W (4,A0,D0.W),D3
        name: "move_indexed",
        code: &[0x3630, 0x0004],
    },
    Kernel {
        // MOVE.W #$1234,($30000).L
        name: "move_immediate_absolute",
        code: &[MOVE_WORD_TO_ABSOLUTE, 0x1234, 0x0003, 0x0000],
    },
    Kernel {
        // ADD.L D0,D1
        name: "add_register",
        code: &[0xd280],
    },
    Kernel {
        // ADD.W D1,(A1)
        name: "add_to_memory",
        code: &[0xd351],
    },
    Kernel {
        // EOR.L D1,D0
        name: "eor_register",
        code: &[0xb380],
    },
    Kernel {
        // ADDA.L D0,A2
        name: "adda",
        code: &[0xd5c0],
    },
    Kernel {
        // ADDQ.L #1,D0
        name: "addq",
        code: &[0x5280],
    },
    Kernel {
        // ADDI.L #1,D0
        name: "addi",
        code: &[0x0680, 0x0000, 0x0001],
    },
    Kernel {
        // CMPI.W #0,D0
        name: "cmpi",
        code: &[0x0c40, 0x0000],
    },
    Kernel {
        // CMP.L D0,D1
        name: "cmp_register",
        code: &[0xb280],
    },
    Kernel {
        // TST.L D0
        name: "tst",
        code: &[0x4a80],
    },
    Kernel {
        // NEG.L D0
        name: "neg",
        code: &[0x4480],
    },
    Kernel {
        // CLR.W (A1)
        name: "clr_indirect",
        code: &[0x4251],
    },
    Kernel {
        // LSL.L #1,D0
        name: "lsl_immediate",
        code: &[0xe388],
    },
    Kernel {
        // LSR.L D1,D0
        name: "lsr_register",
        code: &[0xe2a8],
    },
    Kernel {
        // BTST #3,D0
        name: "btst_immediate",
        code: &[0x0800, 0x0003],
    },
    Kernel {
        // BSET D1,(A1)
        name: "bset_memory",
        code: &[0x03d1],
    },
    Kernel {
        // MULU.W D1,D0
        name: "mulu",
        code: &[0xc0c1],
    },
    Kernel {
        // LEA (16,A0),A2
        name: "lea_displacement",
        code: &[0x45e8, 0x0010],
    },
    Kernel {
        // PEA (16,A0)
        name: "pea_displacement",
        code: &[0x4868, 0x0010],
    },
    Kernel {
        // BRA.S to itself
        name: "bra_self",
        code: &[0x60fe],
    },
    Kernel {
        // DBRA D7 to itself, 65536 times round
        name: "dbra_self",
        code: &[0x51cf, 0xfffe],
    },
];

/// `kernel` repeated to fill `bytes` bytes, big-endian.
#[must_use]
pub fn sled(kernel: &Kernel, bytes: usize) -> Vec<u8> {
    let words: Vec<u8> = kernel
        .code
        .iter()
        .flat_map(|word| word.to_be_bytes())
        .collect();
    words.iter().copied().cycle().take(bytes).collect()
}

/// Whether `exception` says the core lacks an instruction: an illegal
/// instruction or an emulator line (CPU32RM 6.2.8).
#[must_use]
pub fn unimplemented(exception: Exception) -> bool {
    matches!(
        exception.vector(),
        Vector::ILLEGAL_INSTRUCTION | Vector::LINE_A | Vector::LINE_F
    )
}

/// What one benchmark iteration does: counted one instruction at a
/// time in a calibration run, then checked against a run of the whole
/// budget at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Calibration {
    /// Instructions, interrupt entries among them.
    pub instructions: u64,
    /// Emulated clocks.
    pub clocks: Cycles,
}

/// Why a kernel cannot be measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// The core does not have one of its instructions yet.
    Unimplemented,
}

impl Calibration {
    /// Counts the instructions `step` runs, one each call, until their
    /// clocks reach `budget`.  `step` runs one instruction and returns
    /// its clocks and the exception it took.  Exceptions other than an
    /// unimplemented instruction are left to the caller to allow.
    pub fn measure(
        budget: Cycles,
        mut step: impl FnMut() -> (Cycles, Option<Exception>),
    ) -> Result<Self, Skip> {
        let mut calibration = Self {
            instructions: 0,
            clocks: Cycles::ZERO,
        };
        while calibration.clocks < budget {
            let (clocks, exception) = step();
            if exception.is_some_and(unimplemented) {
                return Err(Skip::Unimplemented);
            }
            calibration.instructions += 1;
            calibration.clocks += clocks;
        }
        Ok(calibration)
    }

    /// Emulated clocks per instruction.
    #[must_use]
    pub fn clocks_per_instruction(&self) -> f64 {
        self.clocks.0 as f64 / self.instructions as f64
    }

    /// The calibration line a benchmark prints, so emulated clock speed
    /// follows from its instructions per second.
    #[must_use]
    pub fn describe(&self, name: &str) -> String {
        format!(
            "{name}: {} instructions, {} clocks per iteration; {:.2} clocks per instruction",
            self.instructions,
            self.clocks.0,
            self.clocks_per_instruction()
        )
    }
}
