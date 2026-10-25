//! Musashi (github.com/kstenerud/Musashi, MIT) in 68020 mode, as a
//! black-box oracle: set the registers, run one instruction, read them
//! back; or run a firmware call on its memory's regions until it ends.
//! It has no CPU32 model; the 68020 is the nearest.  Only its public API
//! (`m68k.h`) is used.  Musashi is one global CPU, so one test at a time
//! holds it.

use std::array;
use std::cell::RefCell;
use std::ffi::c_int;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::Once;
use std::sync::PoisonError;

use crate::call::Call;
use crate::call::CallEnd;
use crate::call::Ending;
use crate::call::FORMAT_WORD_OFFSET;
use crate::call::SUPERVISOR;
use crate::call::TRAP;
use crate::call::TRAP_MASK;
use crate::call::TRAP_VECTOR;
use crate::call::VECTOR_BYTES;
use crate::call::VECTOR_OFFSET_MASK;
use crate::call::stop_before;
use crate::memory::RegionMemory;
use crate::memory::SparseMemory;
use crate::state::State;
/// NOP's opcode (CPU32RM 4.4, NOP).
const NOP: u16 = 0x4e71;
/// The stack pointer `reset_memory` gives the reset.
const RESET_STACK: u16 = 0x0800;
/// Where `reset_memory`'s NOP is.
const RESET_PC: u16 = 0x0400;

/// The memory Musashi runs on: an instruction test's, or a call's.
#[derive(Debug)]
enum Backing {
    /// An instruction test's.
    Sparse(SparseMemory),
    /// A call's.
    Regions(RegionMemory),
}

impl Backing {
    fn read_byte(&mut self, address: u32) -> u8 {
        match self {
            Self::Sparse(memory) => memory.read_byte(address),
            Self::Regions(memory) => memory.read_byte(address),
        }
    }

    fn write_byte(&mut self, address: u32, value: u8) {
        match self {
            Self::Sparse(memory) => memory.write_byte(address, value),
            Self::Regions(memory) => memory.write_byte(address, value),
        }
    }

    fn read_word(&mut self, address: u32) -> u16 {
        u16::from_be_bytes([
            self.read_byte(address),
            self.read_byte(address.wrapping_add(1)),
        ])
    }

    fn write_word(&mut self, address: u32, value: u16) {
        let [high, low] = value.to_be_bytes();
        self.write_byte(address, high);
        self.write_byte(address.wrapping_add(1), low);
    }

    /// The call's memory, which a call runs on.
    fn regions(&self) -> &RegionMemory {
        match self {
            Self::Regions(memory) => memory,
            Self::Sparse(_) => unreachable!("a call runs on regions"),
        }
    }
}

thread_local! {
    /// The memory Musashi's callbacks read and write, on the thread
    /// that holds `LOCK`.
    static MEMORY: RefCell<Backing> = const { RefCell::new(Backing::Sparse(SparseMemory::new(0))) };
}

/// Held while a test drives Musashi's one global CPU.
static LOCK: Mutex<()> = Mutex::new(());
/// Builds Musashi's opcode table once.
static INIT: Once = Once::new();

/// Musashi's C API and the memory callbacks it calls.
#[expect(unsafe_code, reason = "the FFI to Musashi, the only unsafe code")]
mod ffi {
    use std::ffi::c_int;
    use std::ffi::c_uint;
    use std::ffi::c_void;

    use super::MEMORY;

    /// `M68K_CPU_TYPE_68020` (m68k.h).
    pub(super) const CPU_TYPE_68020: c_uint = 4;

    /// `m68k_register_t` (m68k.h), the registers used here.
    pub(super) mod register {
        use std::ffi::c_int;

        /// `M68K_REG_D0`; D1-D7 follow.
        pub(crate) const D0: c_int = 0;
        /// `M68K_REG_A0`; A1-A7 follow.
        pub(crate) const A0: c_int = 8;
        /// `M68K_REG_A7`.
        pub(crate) const A7: c_int = 15;
        /// `M68K_REG_PC`.
        pub(crate) const PC: c_int = 16;
        /// `M68K_REG_SR`.
        pub(crate) const SR: c_int = 17;
        /// `M68K_REG_USP`.
        pub(crate) const USP: c_int = 19;
        /// `M68K_REG_ISP`.
        pub(crate) const ISP: c_int = 20;
        /// `M68K_REG_VBR`.
        pub(crate) const VBR: c_int = 24;
    }

    unsafe extern "C" {
        pub(super) fn m68k_init();
        pub(super) fn m68k_set_cpu_type(cpu_type: c_uint);
        pub(super) fn m68k_pulse_reset();
        pub(super) fn m68k_execute(cycles: c_int) -> c_int;
        pub(super) fn m68k_get_reg(context: *mut c_void, register: c_int) -> c_uint;
        pub(super) fn m68k_set_reg(register: c_int, value: c_uint);
    }

    // Musashi's memory interface (m68k.h).  The 32-bit accesses are
    // big-endian pairs of words, as the bus would split them.

    #[unsafe(no_mangle)]
    extern "C" fn m68k_read_memory_8(address: c_uint) -> c_uint {
        MEMORY.with_borrow_mut(|memory| c_uint::from(memory.read_byte(address)))
    }

    #[unsafe(no_mangle)]
    extern "C" fn m68k_read_memory_16(address: c_uint) -> c_uint {
        MEMORY.with_borrow_mut(|memory| c_uint::from(memory.read_word(address)))
    }

    #[unsafe(no_mangle)]
    extern "C" fn m68k_read_memory_32(address: c_uint) -> c_uint {
        MEMORY.with_borrow_mut(|memory| {
            c_uint::from(memory.read_word(address)) << 16
                | c_uint::from(memory.read_word(address.wrapping_add(2)))
        })
    }

    #[unsafe(no_mangle)]
    extern "C" fn m68k_write_memory_8(address: c_uint, value: c_uint) {
        MEMORY.with_borrow_mut(|memory| memory.write_byte(address, value as u8));
    }

    #[unsafe(no_mangle)]
    extern "C" fn m68k_write_memory_16(address: c_uint, value: c_uint) {
        MEMORY.with_borrow_mut(|memory| memory.write_word(address, value as u16));
    }

    #[unsafe(no_mangle)]
    extern "C" fn m68k_write_memory_32(address: c_uint, value: c_uint) {
        MEMORY.with_borrow_mut(|memory| {
            memory.write_word(address, (value >> 16) as u16);
            memory.write_word(address.wrapping_add(2), value as u16);
        });
    }

    /// Sets a register.
    pub(super) fn set(register: c_int, value: u32) {
        // SAFETY: Musashi is initialized (`Musashi::lock`) and the
        // caller holds `LOCK`, so no other thread touches its globals.
        unsafe { m68k_set_reg(register, value) }
    }

    /// Reads a register of the running context.
    pub(super) fn get(register: c_int) -> u32 {
        // SAFETY: as `set`; a null context reads the running CPU's.
        unsafe { m68k_get_reg(std::ptr::null_mut(), register) }
    }

    /// Builds Musashi's tables and makes it a 68020.
    pub(super) fn init() {
        // SAFETY: called once, under `LOCK`.
        unsafe {
            m68k_init();
            m68k_set_cpu_type(CPU_TYPE_68020);
        }
    }

    /// Pulses RESET, clearing whatever the last test left: a stop, a
    /// halt.  It reads its vectors from `MEMORY`.
    pub(super) fn reset() {
        // SAFETY: under `LOCK`, after `init`; the callbacks it makes
        // only touch this thread's `MEMORY`.
        unsafe { m68k_pulse_reset() }
    }

    /// Runs one instruction: the smallest budget, which any instruction
    /// overruns.
    pub(super) fn step() {
        // SAFETY: as `reset`.
        unsafe {
            m68k_execute(1);
        }
    }
}

/// The reset vectors and the NOP they point to, which the first run
/// after a reset may execute.
fn reset_memory() -> Backing {
    let mut memory = SparseMemory::new(0);
    for (address, word) in [
        (0, 0),
        (2, RESET_STACK),
        (4, 0),
        (6, RESET_PC),
        (u32::from(RESET_PC), NOP),
    ] {
        memory.write_word(address, word);
    }
    Backing::Sparse(memory)
}

/// The right to drive Musashi, one test at a time.
#[derive(Debug)]
pub struct Musashi {
    /// Held for as long as this lives.
    _lock: MutexGuard<'static, ()>,
}

impl Musashi {
    /// Waits for Musashi, initializing it the first time.
    #[must_use]
    pub fn lock() -> Self {
        let lock = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        INIT.call_once(ffi::init);
        Self { _lock: lock }
    }

    /// Runs one instruction from `state` on `memory`, as a 68020 with
    /// VBR zero: the registers and memory after it.
    pub fn step(&mut self, state: &State, memory: SparseMemory) -> (State, SparseMemory) {
        begin(state, 0, Backing::Sparse(memory));
        ffi::step();
        let state = capture();
        match MEMORY.replace(Backing::Sparse(SparseMemory::new(0))) {
            Backing::Sparse(memory) => (state, memory),
            Backing::Regions(_) => unreachable!("a step runs on sparse memory"),
        }
    }

    /// Runs `call` on `memory` until it ends.  Musashi takes the
    /// exceptions itself, a handled trap's among them, as a 68020; one
    /// it takes that is not a handled trap ends the call, as does an
    /// access no region answers, once its instruction is done.
    pub fn call(&mut self, call: &Call<'_>, memory: RegionMemory) -> CallEnd {
        begin(&call.state, call.vbr, Backing::Regions(memory));
        let word = |at: u32| MEMORY.with_borrow(|memory| memory.regions().peek_word(at));
        let mut steps = 0;
        let ending = loop {
            let pc = ffi::get(ffi::register::PC);
            if pc == call.until {
                break Ending::Returned;
            }
            if let Some(ending) = stop_before(word, pc, call.handled_traps) {
                break ending;
            }
            if steps == call.step_limit {
                break Ending::StepLimit;
            }
            let opcode = word(pc);
            let stack = ffi::get(ffi::register::A7);
            ffi::step();
            steps += 1;
            if let Some(fault) = MEMORY.with_borrow(|memory| memory.regions().fault()) {
                break Ending::Fault(fault);
            }
            if let Some(vector) = exception_taken(&word, stack) {
                let trap = opcode
                    .filter(|opcode| opcode & TRAP_MASK == TRAP)
                    .map(|opcode| TRAP_VECTOR + (opcode & !TRAP_MASK) as u8);
                if trap != Some(vector) {
                    break Ending::Exception { vector, pc };
                }
            }
        };
        let state = capture();
        let Backing::Regions(memory) = MEMORY.replace(Backing::Sparse(SparseMemory::new(0))) else {
            unreachable!("a call runs on regions");
        };
        CallEnd {
            ending,
            state,
            steps,
            memory,
        }
    }
}

/// Starts Musashi afresh on `memory` with registers `state` and VBR
/// `vbr`.  Each start begins with a reset, so nothing carries over from
/// the last (a stop, a halt).  The first run after a reset spends the
/// reset's own cycles, not an instruction, so a run on `reset_memory`
/// comes first.
fn begin(state: &State, vbr: u32, memory: Backing) {
    MEMORY.set(reset_memory());
    ffi::reset();
    ffi::step();
    MEMORY.set(memory);
    ffi::set(ffi::register::SR, u32::from(state.sr));
    for (number, value) in (0..).zip(state.data) {
        ffi::set(ffi::register::D0 + number, value);
    }
    for (number, value) in (0..).zip(state.address) {
        ffi::set(ffi::register::A0 + number, value);
    }
    ffi::set(ffi::register::USP, state.usp);
    ffi::set(ffi::register::ISP, state.ssp);
    let supervisor = state.sr & SUPERVISOR != 0;
    let a7 = if supervisor { state.ssp } else { state.usp };
    ffi::set(ffi::register::A7, a7);
    ffi::set(ffi::register::VBR, vbr);
    ffi::set(ffi::register::PC, state.pc);
}

/// Musashi's registers.
fn capture() -> State {
    let sr = ffi::get(ffi::register::SR) as u16;
    let a7 = ffi::get(ffi::register::A7);
    let (usp, ssp) = if sr & SUPERVISOR != 0 {
        (ffi::get(ffi::register::USP), a7)
    } else {
        (a7, ffi::get(ffi::register::ISP))
    };
    State {
        data: array::from_fn(|n| ffi::get(ffi::register::D0 + n as c_int)),
        address: array::from_fn(|n| ffi::get(ffi::register::A0 + n as c_int)),
        usp,
        ssp,
        sr,
        pc: ffi::get(ffi::register::PC),
    }
}

/// The vector of the exception the last instruction took, if it took
/// one, given A7 before it: Musashi has no exception callback, so an
/// instruction took one when it ends in supervisor state at the handler
/// of the vector its frame's format/vector word names, the frame below
/// the stack it started with (CPU32RM 6.1.3, 6.1.4).
fn exception_taken(word: &impl Fn(u32) -> Option<u16>, stack: u32) -> Option<u8> {
    let sr = ffi::get(ffi::register::SR) as u16;
    let a7 = ffi::get(ffi::register::A7);
    if sr & SUPERVISOR == 0 || a7 >= stack {
        return None;
    }
    let format = word(a7 + FORMAT_WORD_OFFSET)?;
    let vector = u32::from(format & VECTOR_OFFSET_MASK) / VECTOR_BYTES;
    let at = ffi::get(ffi::register::VBR).wrapping_add(vector * VECTOR_BYTES);
    let handler = u32::from(word(at)?) << 16 | u32::from(word(at + 2)?);
    (ffi::get(ffi::register::PC) == handler).then_some(vector as u8)
}

#[cfg(test)]
mod tests {
    use super::Musashi;
    use crate::call::Ending;
    use crate::call::sample;
    use crate::memory::Fault;
    use crate::memory::SparseMemory;
    use crate::state::State;

    #[test]
    fn a_call_runs_its_handled_trap_and_returns() {
        let mut musashi = Musashi::lock();
        let end = musashi.call(&sample::call(&[1]), sample::memory(sample::STORE));
        assert_eq!(end.ending, Ending::Returned);
        assert_eq!(end.state.data[0], 6);
        assert_eq!(end.state.ssp, sample::STACK + 4);
        assert_eq!(end.memory.peek_word(sample::RAM), Some(6));
        assert_eq!(end.steps, 6);
    }

    #[test]
    fn a_call_ends_at_an_unhandled_trap_and_a_fault() {
        let mut musashi = Musashi::lock();
        let end = musashi.call(&sample::call(&[]), sample::memory(sample::STORE));
        assert_eq!(end.ending, Ending::Trap(1));
        assert_eq!(end.state.pc, sample::CODE + 2);
        let end = musashi.call(&sample::call(&[1]), sample::memory(sample::STORE_TO_ROM));
        assert_eq!(
            end.ending,
            Ending::Fault(Fault {
                address: 0x100,
                write: true
            })
        );
    }

    #[test]
    fn runs_exactly_one_instruction() {
        // MOVEQ #3, D1; MOVEQ #4, D2
        let mut memory = SparseMemory::new(1);
        memory.write_word(0x1000, 0x7203);
        memory.write_word(0x1002, 0x7404);
        let state = State {
            sr: 0x2700,
            pc: 0x1000,
            ssp: 0x2000,
            ..State::default()
        };
        let mut musashi = Musashi::lock();
        for _ in 0..2 {
            let (after, _) = musashi.step(&state, memory.clone());
            assert_eq!(after.pc, 0x1002);
            assert_eq!(after.data[1], 3);
            assert_eq!(after.data[2], 0);
            assert_eq!(after.ssp, 0x2000);
        }
    }
}
