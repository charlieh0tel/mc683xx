"""The bare CPU from Python: flat memory at any address, peripherals,
interrupts, limited runs, and hooks that change the CPU, each write's
timing as docs/python.md (Bare CPU) says."""

import struct
import time

import pytest

import mc683xx

PAGE = 2048
RAM_BYTES = 64 << 10
STACK = 0x8000
# Code above 24 bits, as smartclock's harness puts its routines.
CODE = 0x31400000
# A return address nothing maps.
RETURN = 0x800000
HANDLER = 0x4000
PERIPHERAL = 0xF00000

# Vector numbers (CPU32RM Table 6-1).
ILLEGAL_INSTRUCTION = 4
TRAP_0 = 32

# Opcodes (CPU32RM 4).
NOP = 0x4E71
RTS = 0x4E75
ILLEGAL = 0x4AFC
MOVE_WORD_A0_TO_D0 = 0x3010
MOVE_WORD_D0_TO_A0 = 0x3080
MOVE_LONG_A0_TO_D0 = 0x2010
MOVE_LONG_D0_TO_A0 = 0x2080
ADDQ_LONG_1_D0 = 0x5280
SUPERVISOR = 0x2000


def moveq(value, register):
    return 0x7000 | register << 9 | value


def trap(number):
    return 0x4E40 | number


def words(*values):
    return struct.pack(f">{len(values)}H", *values)


def machine(*code):
    """RAM from zero, every vector at HANDLER, `code` at CODE, SR
    supervisor, a call's frame: RETURN at the top of the stack."""
    cpu = mc683xx.Machine()
    cpu.map(0, RAM_BYTES)
    cpu.write(8, struct.pack(">I", HANDLER) * 254)
    cpu.map(CODE, PAGE, permissions="rx", contents=words(*code))
    cpu.write(STACK - 4, struct.pack(">I", RETURN))
    cpu.write_register("SR", SUPERVISOR)
    cpu.write_register("A7", STACK - 4)
    cpu.write_register("PC", CODE)
    return cpu


def test_a_call_runs_until_it_returns():
    cpu = machine(moveq(5, 0), ADDQ_LONG_1_D0, RTS)
    run = cpu.run_limited(until=RETURN)
    assert run.end == mc683xx.End.Until()
    assert cpu.read_register("D0") == 6
    assert cpu.read_register("PC") == RETURN
    assert cpu.instructions == 3
    assert run.cycles > 0


def test_limits_and_begin():
    cpu = machine(NOP, NOP, NOP, RTS)
    assert cpu.run_limited(instructions=2).end == mc683xx.End.Instructions()
    assert cpu.read_register("PC") == CODE + 4
    assert cpu.run_limited(cycles=1).end == mc683xx.End.Stop(mc683xx.Stop.Budget())
    assert cpu.run_limited(begin=CODE, until=RETURN).end == mc683xx.End.Until()


def test_memory_maps_and_refusals():
    cpu = machine(NOP)
    with pytest.raises(ValueError):
        cpu.map(RAM_BYTES - PAGE, 2 * PAGE)
    with pytest.raises(ValueError):
        cpu.map(1, PAGE)
    with pytest.raises(mc683xx.BusError):
        cpu.read(RAM_BYTES - 2, 4)
    # Refused before a byte is allocated.
    with pytest.raises(mc683xx.BusError):
        cpu.read(0, 2**32 - 1)
    with pytest.raises(ValueError):
        cpu.read(0, 2**40)
    cpu.write(0x100, b"\x12\x34")
    assert cpu.read(0x100, 2) == b"\x12\x34"
    # Host writes ignore permissions, as a loader's do.
    cpu.write(CODE, words(RTS))
    cpu.unmap(CODE, PAGE)
    with pytest.raises(mc683xx.BusError):
        cpu.read(CODE, 2)


def test_unmap_frees_a_map_or_keeps_the_rest_of_it():
    cpu = machine(RTS)
    contents = bytes(page for page in range(4) for _ in range(PAGE))
    for _ in range(100):
        cpu.map(PERIPHERAL, 4 * PAGE, contents=contents)
        cpu.unmap(PERIPHERAL + PAGE, PAGE)
        assert cpu.read(PERIPHERAL + 2 * PAGE, 1) == b"\x02"
        with pytest.raises(mc683xx.BusError):
            cpu.read(PERIPHERAL + PAGE, 1)
        cpu.unmap(PERIPHERAL, 4 * PAGE)
    with pytest.raises(mc683xx.BusError):
        cpu.read(PERIPHERAL, 1)


def test_a_code_hook_may_unmap_what_the_instruction_reads():
    cpu = machine(MOVE_WORD_A0_TO_D0, RTS)
    cpu.map(PERIPHERAL, PAGE)
    cpu.write_register("A0", PERIPHERAL)
    cpu.set_stop_on_fault(True)
    cpu.add_code_hook(lambda instruction: cpu.unmap(PERIPHERAL, PAGE), CODE, CODE)
    run = cpu.run_limited(until=RETURN)
    assert isinstance(run.end, mc683xx.End.Fault)
    assert run.end._0.address == PERIPHERAL


def test_a_code_hook_s_pc_write_redirects_before_the_instruction():
    cpu = machine(moveq(1, 0), RTS, moveq(2, 0), RTS)

    def jump(instruction):
        assert instruction.pc == CODE
        cpu.write_register("PC", CODE + 4)

    cpu.add_code_hook(jump, CODE, CODE)
    cpu.run_limited(until=RETURN)
    assert cpu.read_register("D0") == 2


def test_a_code_hook_s_register_write_is_seen_by_the_instruction():
    cpu = machine(ADDQ_LONG_1_D0, RTS)
    cpu.add_code_hook(lambda instruction: cpu.write_register("D0", 41), CODE, CODE)
    cpu.run_limited(until=RETURN)
    assert cpu.read_register("D0") == 42


def test_a_memory_hook_s_writes_land_after_the_instruction():
    cpu = machine(MOVE_WORD_A0_TO_D0, MOVE_WORD_D0_TO_A0, RTS)
    cpu.write_register("A0", 0x2000)
    cpu.write(0x2000, b"\x00\x07")
    seen = []

    def read(access):
        # The instruction has completed: D0 holds what it read.
        assert cpu.read_register("D0") == 7
        assert access.registers is not None and access.registers["D0"] == 0
        assert access.pc == CODE
        cpu.write_register("D0", 9)

    def write(access):
        seen.append((access.value, cpu.read(0x2000, 2), access.stacking))

    cpu.add_read_hook(read, 0x2000, 0x2001)
    cpu.add_write_hook(write, 0x2000, 0x2001)
    cpu.run_limited(until=RETURN)
    assert seen == [(9, b"\x00\x09", False)]


def test_a_memory_hook_sees_a_long_once_and_not_one_that_faults():
    # MOVE.L D0,-(SP); MOVE.L (SP)+,D1; MOVE.L D0,(A0) with A0 at RAM's
    # last word, so its second word is a bus error.
    cpu = machine(0x2F00, 0x221F, MOVE_LONG_D0_TO_A0, RTS)
    cpu.set_stop_on_fault(True)
    cpu.write_register("D0", 0x12345678)
    cpu.write_register("A0", RAM_BYTES - 2)
    seen = []
    for kind, add in (("read", cpu.add_read_hook), ("write", cpu.add_write_hook)):
        add(lambda access, kind=kind: seen.append((kind, access.address, access.size, access.value)))
    run = cpu.run_limited(until=RETURN)
    assert isinstance(run.end, mc683xx.End.Fault)
    assert seen == [
        ("write", STACK - 8, 4, 0x12345678),
        ("read", STACK - 8, 4, 0x12345678),
    ]


def test_a_truthy_hook_or_stop_ends_the_run():
    cpu = machine(NOP, NOP, RTS)
    hook = cpu.add_code_hook(lambda instruction: True, CODE + 2, CODE + 2)
    run = cpu.run_limited(until=RETURN)
    assert run.end == mc683xx.End.Stop(mc683xx.Stop.Hook(hook))
    assert cpu.read_register("PC") == CODE + 2
    assert cpu.remove_hook(hook)
    stop = cpu.stop_request()
    cpu.add_code_hook(lambda instruction: stop.request(), CODE + 4, CODE + 4)
    assert cpu.run_limited(until=RETURN).end == mc683xx.End.Stop(mc683xx.Stop.Requested())
    assert cpu.read_register("PC") == CODE + 4


def test_a_stop_requested_between_runs_is_withdrawn():
    cpu = machine(NOP, NOP, RTS)
    cpu.stop_request().request()
    assert cpu.run_limited(until=RETURN).end == mc683xx.End.Until()


def test_lpstop_stops_the_bare_cpu():
    # LPSTOP #$2700 (CPU32RM 4.4): the Machine acknowledges its broadcast.
    cpu = machine(0xF800, 0x01C0, 0x2700)
    assert cpu.run_limited().end == mc683xx.End.Stop(mc683xx.Stop.Stopped())
    assert (cpu.read_register("SR"), cpu.read_register("PC")) == (0x2700, CODE + 6)


def test_a_hook_s_exception_is_raised_and_a_nested_run_refused():
    cpu = machine(NOP, RTS)

    def nested(instruction):
        cpu.run_limited(instructions=1)

    cpu.add_code_hook(nested, CODE, CODE)
    with pytest.raises(RuntimeError):
        cpu.run_limited(until=RETURN)


def test_an_exception_hook_may_return_from_the_handler():
    cpu = machine(trap(3), RTS)
    entries = []

    def entered(entry):
        entries.append(entry.vector)
        # rte by hand: pop the four-word frame (CPU32RM 6.4).
        sp = cpu.read_register("SSP")
        sr, pc = struct.unpack(">HI", cpu.read(sp, 6))
        cpu.write_register("SSP", sp + 8)
        cpu.write_register("SR", sr)
        cpu.write_register("PC", pc)

    cpu.add_exception_hook(entered)
    assert cpu.run_limited(until=RETURN).end == mc683xx.End.Until()
    assert entries == [TRAP_0 + 3]


def test_an_intercept_handles_or_declines_before_stacking():
    cpu = machine(trap(1), trap(7), RTS)
    seen = []

    def answer(intercepted):
        seen.append((intercepted.vector, intercepted.pc))
        if intercepted.vector == TRAP_0 + 1:
            cpu.write_register("PC", intercepted.pc + 2)
            return True
        return False

    cpu.set_intercepted(TRAP_0 + 1, True)
    cpu.set_intercepted(TRAP_0 + 7, True)
    cpu.set_intercept_callback(answer)
    run = cpu.run_limited(until=HANDLER)
    assert run.end == mc683xx.End.Until()
    assert seen == [(TRAP_0 + 1, CODE), (TRAP_0 + 7, CODE + 2)]
    # Declined, trap #7 was taken natively: its frame is stacked.
    assert cpu.read_register("SSP") == STACK - 4 - 8
    cpu.set_intercept_callback(None)


def test_an_intercepted_exception_with_no_callback_is_taken_natively():
    cpu = machine(trap(1), RTS)
    cpu.set_intercepted(TRAP_0 + 1, True)
    assert cpu.run_limited(until=HANDLER).end == mc683xx.End.Until()
    assert cpu.read_register("SSP") == STACK - 4 - 8


def test_a_fault_stops_the_run_with_nothing_stacked():
    cpu = machine(moveq(3, 1), MOVE_LONG_A0_TO_D0, RTS)
    cpu.set_stop_on_fault(True)
    cpu.write_register("A0", 0x90000000)
    match cpu.run_limited(until=RETURN).end:
        case mc683xx.End.Fault(fault):
            pass
        case end:
            raise AssertionError(end)
    assert (fault.cycle, fault.cause) == (mc683xx.CycleKind.READ, mc683xx.FaultCause.BUS_ERROR)
    assert (fault.address, fault.size, fault.pc) == (0x90000000, 4, CODE + 2)
    assert fault.registers["D1"] == 3
    assert cpu.read_register("PC") == CODE + 2
    assert cpu.read_register("SSP") == STACK - 4


def test_a_write_to_read_only_code_is_protected():
    cpu = machine(MOVE_WORD_D0_TO_A0, RTS)
    cpu.set_stop_on_fault(True)
    cpu.write_register("A0", CODE)
    end = cpu.run_limited(until=RETURN).end
    assert isinstance(end, mc683xx.End.Fault)
    assert (end._0.cycle, end._0.cause) == (mc683xx.CycleKind.WRITE, mc683xx.FaultCause.PROTECTED)
    assert end == mc683xx.End.Fault(end._0)


class Register:
    """A peripheral: a long register, and a log of its cycles."""

    def __init__(self):
        self.value = 0x11223344
        self.cycles = []

    def read(self, cycle):
        self.cycles.append(("read", cycle.address, cycle.size))
        return self.value if cycle.size == 4 else self.value & 0xFFFF

    def write(self, cycle, value):
        self.cycles.append(("write", cycle.address, cycle.size))
        self.value = value


def test_a_peripheral_takes_a_long_whole_before_it_lands():
    cpu = machine(MOVE_LONG_A0_TO_D0, ADDQ_LONG_1_D0, MOVE_LONG_D0_TO_A0, RTS)
    register = Register()
    cpu.add_peripheral(PERIPHERAL, PERIPHERAL + 3, register)
    cpu.write_register("A0", PERIPHERAL)
    cpu.run_limited(until=RETURN)
    assert register.cycles == [("read", PERIPHERAL, 4), ("write", PERIPHERAL, 4)]
    assert register.value == 0x11223345


class Refusing:
    def read(self, cycle):
        raise mc683xx.BusError()

    def write(self, cycle, value):
        raise KeyError("broken")


def test_a_peripheral_s_errors():
    cpu = machine(MOVE_WORD_A0_TO_D0, MOVE_WORD_D0_TO_A0, RTS)
    cpu.add_peripheral(PERIPHERAL, PERIPHERAL + 1, Refusing())
    cpu.write_register("A0", PERIPHERAL)
    cpu.set_stop_on_fault(True)
    assert isinstance(cpu.run_limited(until=RETURN).end, mc683xx.End.Fault)
    cpu.write_register("PC", CODE + 2)
    with pytest.raises(KeyError):
        cpu.run_limited(until=RETURN)


def test_reset_raises_a_peripheral_s_exception_at_once():
    class Vectors:
        failing = True

        def read(self, cycle):
            if self.failing:
                raise KeyError("no vectors")
            return NOP

        def write(self, cycle, value):
            pass

    vectors = Vectors()
    cpu = mc683xx.Machine()
    cpu.add_peripheral(0, 7, vectors)
    with pytest.raises(KeyError):
        cpu.reset()
    # the bus not faulted: the vectors read undriven, 0
    assert cpu.halted is None
    assert (cpu.read_register("SSP"), cpu.read_register("PC")) == (0, 0)
    # Nothing is left waiting for the next call.
    vectors.failing = False
    assert cpu.run_limited(instructions=1).end == mc683xx.End.Instructions()


def test_an_interrupt_request_is_taken_through_its_vector():
    cpu = machine(NOP, RTS)
    cpu.write(64 * 4, struct.pack(">I", HANDLER))
    cpu.write(HANDLER, words(NOP))
    cpu.request_interrupt(5, 64)
    assert cpu.run_limited(until=HANDLER).end == mc683xx.End.Until()
    assert cpu.read_register("SR") >> 8 & 7 == 5
    cpu.clear_interrupt(5)
    with pytest.raises(ValueError):
        cpu.request_interrupt(0)


def test_the_parity_switches():
    cpu = machine(MOVE_WORD_A0_TO_D0, RTS)
    cpu.write_register("A0", 0x2001)
    cpu.write(0x2001, b"\xab\xcd")
    deviations = mc683xx.Deviations(misaligned=True, untraced=True)
    cpu.set_deviations(deviations)
    assert cpu.deviations == deviations
    cpu.write_register("SR", SUPERVISOR | 0x8000)
    assert cpu.run_limited(until=RETURN).end == mc683xx.End.Until()
    assert cpu.read_register("D0") == 0xABCD


def test_save_and_restore():
    cpu = machine(moveq(1, 0), RTS)
    snapshot = cpu.save()
    cpu.run_limited(until=RETURN)
    cpu.write(0x100, b"\x01")
    cpu.restore(snapshot)
    assert cpu.read_register("PC") == CODE
    assert cpu.read(0x100, 1) == b"\x00"
    with pytest.raises(mc683xx.StateError):
        mc683xx.Machine(address_bits=24).restore(snapshot)


class Latch:
    """A peripheral that reads as the last value written, which it saves."""

    def __init__(self, value=0):
        self.value = value

    def read(self, cycle):
        return self.value & ((1 << 8 * cycle.size) - 1)

    def write(self, cycle, value):
        self.value = value

    def save(self):
        return struct.pack(">I", self.value)

    def restore(self, snapshot):
        (self.value,) = struct.unpack(">I", snapshot)


class Unsaved:
    """A peripheral with no snapshot."""

    def read(self, cycle):
        return 0

    def write(self, cycle, value):
        pass


def test_a_snapshot_holds_the_peripherals():
    cpu = machine(moveq(1, 0), RTS)
    first, second = Latch(1), Latch(2)
    cpu.add_peripheral(PERIPHERAL + 0x100, PERIPHERAL + 0x103, second)
    cpu.add_peripheral(PERIPHERAL, PERIPHERAL + 3, first)
    snapshot = cpu.save()
    first.value, second.value = 10, 20
    cpu.restore(snapshot)
    assert (first.value, second.value) == (1, 2)


def test_a_peripheral_without_save_fails_the_save_naming_it():
    cpu = machine(RTS)
    cpu.add_peripheral(PERIPHERAL, PERIPHERAL + 3, Latch())
    snapshot = cpu.save()
    cpu.add_peripheral(PERIPHERAL + 0x100, PERIPHERAL + 0x1FF, Unsaved())
    with pytest.raises(
        mc683xx.StateError, match="^the peripheral at 0xf00100: a Python peripheral cannot save its state$"
    ):
        cpu.save()
    with pytest.raises(mc683xx.StateError, match="peripherals"):
        cpu.restore(snapshot)


def test_a_restore_into_another_count_of_peripherals_is_refused_and_changes_nothing():
    cpu = machine(moveq(1, 0), RTS)
    latch = Latch(1)
    cpu.add_peripheral(PERIPHERAL, PERIPHERAL + 3, latch)
    snapshot = cpu.save()
    cpu.add_peripheral(PERIPHERAL + 0x100, PERIPHERAL + 0x103, Latch(2))
    cpu.run_limited(until=RETURN)
    latch.value = 10
    before = cpu.save()
    with pytest.raises(mc683xx.StateError, match="peripherals"):
        cpu.restore(snapshot)
    assert cpu.save() == before
    assert latch.value == 10


def test_thousands_of_short_calls_a_second():
    # A function test's pattern: a fresh frame per call, run until it
    # returns.  The rate is a floor for a debug build on a loaded host.
    cpu = machine(moveq(1, 0), ADDQ_LONG_1_D0, RTS)
    frame = struct.pack(">I", RETURN)
    calls = 2000
    start = time.perf_counter()
    for _ in range(calls):
        cpu.write(STACK - 4, frame)
        cpu.write_register("A7", STACK - 4)
        cpu.run_limited(begin=CODE, until=RETURN)
    rate = calls / (time.perf_counter() - start)
    print(f"{rate:.0f} calls per second")
    assert rate > 1000
