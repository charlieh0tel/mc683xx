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
RTE = 0x4E73
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
    cpu.poke_bytes(8, struct.pack(">I", HANDLER) * 254)
    cpu.map(CODE, PAGE, permissions="rx", contents=words(*code))
    cpu.poke_bytes(STACK - 4, struct.pack(">I", RETURN))
    cpu.write_register("SR", SUPERVISOR)
    cpu.write_register("A7", STACK - 4)
    cpu.write_register("PC", CODE)
    return cpu


def test_a_call_runs_until_it_returns():
    cpu = machine(moveq(5, 0), ADDQ_LONG_1_D0, RTS)
    run = cpu.run_limited(reach=RETURN)
    assert run.end == mc683xx.End.Reached()
    assert cpu.read_register("D0") == 6
    assert cpu.read_register("PC") == RETURN
    assert cpu.instructions == 3
    assert run.cycles > 0


def test_a_register_takes_32_bits():
    cpu = machine()
    for value in (-1, 1 << 32):
        with pytest.raises(OverflowError):
            cpu.write_register("D0", value)
    cpu.write_register("D0", -1 & 0xFFFFFFFF)
    assert cpu.read_register("D0") == 0xFFFFFFFF


def test_a_write_hook_sees_stacking_flagged():
    cpu = machine(trap(0))
    writes = []
    cpu.add_write_hook(lambda access: writes.append(access.stacking))
    run = cpu.run_limited(reach=HANDLER)
    assert run.end == mc683xx.End.Reached()
    # the four-word frame, a word a cycle (CPU32RM 6.4.1), and no
    # write of the program's own
    assert writes == [True] * 4


def test_each_hook_sees_the_number_of_its_instruction() -> None:
    cpu = machine(NOP, MOVE_WORD_D0_TO_A0, trap(0))
    cpu.write_register("A0", 0x2000)
    seen: list[tuple[str, int]] = []
    cpu.add_code_hook(lambda instruction: seen.append(("code", instruction.instruction)))
    cpu.add_write_hook(lambda access: seen.append(("write", access.instruction)))
    cpu.add_pre_write_hook(lambda write, data: seen.append(("pre", write.instruction)))
    cpu.run_limited(reach=HANDLER)
    assert cpu.instructions == 3
    assert seen[:5] == [("code", 0), ("code", 1), ("pre", 1), ("write", 1), ("code", 2)]
    # the TRAP's frame, part of the TRAP, number 2
    assert {seen_by for seen_by, _ in seen[5:]} == {"pre", "write"}
    assert all(number == 2 for _, number in seen[5:])


def test_limits_and_begin():
    cpu = machine(NOP, NOP, NOP, RTS)
    assert cpu.run_limited(instructions=2).end == mc683xx.End.Instructions()
    assert cpu.read_register("PC") == CODE + 4
    end = cpu.run_limited(cycles=1).end
    assert end == mc683xx.End.Stop(mc683xx.Stop.Budget())
    assert isinstance(end, mc683xx.End.Stop) and end.stop == mc683xx.Stop.Budget()
    assert repr(end) == "End.Stop(Stop.Budget())"
    run = machine(NOP, RTS).run_limited(instructions=1)
    assert repr(run) == f"LimitedRun(cycles={run.cycles}, end=End.Instructions())"
    halted = mc683xx.Stop.Halted(mc683xx.Halt.DOUBLE_BUS_FAULT)
    assert repr(halted) == "Stop.Halted(Halt.DOUBLE_BUS_FAULT)"
    entry = mc683xx.Stop.Background(mc683xx.Entry.DoubleBusFault(0x1234))
    assert repr(entry) == "Stop.Background(Entry.DoubleBusFault(ssw=0x1234))"
    assert halted.halt == mc683xx.Halt.DOUBLE_BUS_FAULT
    assert entry.entry == mc683xx.Entry.DoubleBusFault(0x1234)
    match entry:
        case mc683xx.Stop.Background(background):
            assert background == entry.entry
        case _:
            pytest.fail("Stop.Background did not match positionally")
    assert cpu.run_limited(begin=CODE, reach=RETURN).end == mc683xx.End.Reached()


def test_memory_maps_and_refusals():
    cpu = machine(NOP)
    with pytest.raises(ValueError):
        cpu.map(RAM_BYTES - PAGE, 2 * PAGE)
    with pytest.raises(ValueError):
        cpu.map(1, PAGE)
    with pytest.raises(mc683xx.BusError):
        cpu.peek_bytes(RAM_BYTES - 2, 4)
    # Refused before a byte is allocated.
    with pytest.raises(mc683xx.BusError):
        cpu.peek_bytes(0, 2**32 - 1)
    with pytest.raises(ValueError):
        cpu.peek_bytes(0, 2**40)
    cpu.poke_bytes(0x100, b"\x12\x34")
    assert cpu.peek_bytes(0x100, 2) == b"\x12\x34"
    # Host writes ignore permissions, as a loader's do.
    cpu.poke_bytes(CODE, words(RTS))
    cpu.unmap(CODE, PAGE)
    with pytest.raises(mc683xx.BusError):
        cpu.peek_bytes(CODE, 2)


def test_unmap_frees_a_map_or_keeps_the_rest_of_it():
    cpu = machine(RTS)
    contents = bytes(page for page in range(4) for _ in range(PAGE))
    for _ in range(100):
        cpu.map(PERIPHERAL, 4 * PAGE, contents=contents)
        cpu.unmap(PERIPHERAL + PAGE, PAGE)
        assert cpu.peek_bytes(PERIPHERAL + 2 * PAGE, 1) == b"\x02"
        with pytest.raises(mc683xx.BusError):
            cpu.peek_bytes(PERIPHERAL + PAGE, 1)
        cpu.unmap(PERIPHERAL, 4 * PAGE)
    with pytest.raises(mc683xx.BusError):
        cpu.peek_bytes(PERIPHERAL, 1)


def test_a_code_hook_may_unmap_what_the_instruction_reads():
    cpu = machine(MOVE_WORD_A0_TO_D0, RTS)
    cpu.map(PERIPHERAL, PAGE)
    cpu.write_register("A0", PERIPHERAL)
    cpu.set_stop_on_fault(True)
    cpu.add_code_hook(lambda instruction: cpu.unmap(PERIPHERAL, PAGE), CODE, CODE)
    run = cpu.run_limited(reach=RETURN)
    assert isinstance(run.end, mc683xx.End.Fault)
    assert run.end.fault.address == PERIPHERAL


def test_a_code_hook_s_pc_write_redirects_before_the_instruction():
    cpu = machine(moveq(1, 0), RTS, moveq(2, 0), RTS)

    def jump(instruction):
        assert instruction.pc == CODE
        cpu.write_register("PC", CODE + 4)

    cpu.add_code_hook(jump, CODE, CODE)
    cpu.run_limited(reach=RETURN)
    assert cpu.read_register("D0") == 2


def test_a_code_hook_s_register_write_is_seen_by_the_instruction():
    cpu = machine(ADDQ_LONG_1_D0, RTS)
    cpu.add_code_hook(lambda instruction: cpu.write_register("D0", 41), CODE, CODE)
    cpu.run_limited(reach=RETURN)
    assert cpu.read_register("D0") == 42


def test_a_memory_hook_s_writes_land_after_the_instruction():
    cpu = machine(MOVE_WORD_A0_TO_D0, MOVE_WORD_D0_TO_A0, RTS)
    cpu.write_register("A0", 0x2000)
    cpu.poke_bytes(0x2000, b"\x00\x07")
    seen = []

    def read(access):
        # The instruction has completed: D0 holds what it read.
        assert cpu.read_register("D0") == 7
        assert access.registers is not None and access.registers["D0"] == 0
        assert access.pc == CODE
        cpu.write_register("D0", 9)

    def write(access):
        seen.append((access.value, cpu.peek_bytes(0x2000, 2), access.stacking))

    cpu.add_read_hook(read, 0x2000, 0x2001)
    cpu.add_write_hook(write, 0x2000, 0x2001)
    cpu.run_limited(reach=RETURN)
    assert seen == [(9, b"\x00\x09", False)]


def test_a_hook_reads_one_register_or_all_as_its_instruction_began():
    cpu = machine(MOVE_WORD_A0_TO_D0, MOVE_WORD_D0_TO_A0, RTS)
    cpu.write_register("A0", 0x2000)
    cpu.write_register("D0", 5)
    cpu.poke_bytes(0x2000, b"\x00\x07")
    seen = []

    def write(access: mc683xx.MemoryAccess) -> None:
        registers = access.registers
        assert registers is not None
        # one dictionary, made once
        assert access.registers is registers
        seen.append((access.register("D0"), access.register("a0"), registers["D0"]))
        assert access.register("A7") == registers["A7"] == STACK - 4
        with pytest.raises(ValueError, match="no register"):
            access.register("D8")

    def code(instruction: mc683xx.Instruction) -> None:
        assert instruction.register("PC") == instruction.pc
        assert instruction.registers is instruction.registers

    cpu.add_write_hook(write, 0x2000, 0x2001)
    cpu.add_code_hook(code)
    cpu.run_limited(reach=RETURN)
    # D0 as the write's instruction began: what the read left
    assert seen == [(7, 0x2000, 7)]


def test_a_pre_read_hook_answers_the_long_from_its_copy_and_peeks_memory():
    cpu = machine(MOVE_LONG_A0_TO_D0, RTS)
    cpu.write_register("A0", 0x2000)
    cpu.poke_bytes(0x2000, struct.pack(">I", 0x11112222))
    cpu.poke_bytes(0x2100, b"\x99")
    seen = []
    kept = []

    def answer(read: mc683xx.PendingRead, data: bytearray) -> None:
        seen.append((read.address, read.size, bytes(data), read.stacking))
        data[3:] = read.peek(0x2100, 1)
        kept.append(read)

    cpu.add_pre_read_hook(answer, 0x2003, 0x2003)
    assert cpu.run_limited(reach=RETURN).end == mc683xx.End.Reached()
    assert cpu.read_register("D0") == 0x11112299
    assert seen == [(0x2000, 4, b"\x11\x11\x22\x22", False)]
    assert cpu.peek_bytes(0x2000, 4) == bytes.fromhex("11112222")
    with pytest.raises(RuntimeError, match="within its hook's call"):
        kept[0].peek(0x2100, 1)


def test_a_pre_read_hook_s_peek_past_memory_raises_and_calling_the_machine_raises():
    cpu = machine(MOVE_WORD_A0_TO_D0, RTS)
    cpu.write_register("A0", 0x2000)
    cpu.add_pre_read_hook(lambda read, data: read.peek(0xF00000, 2), 0x2000, 0x2001)
    with pytest.raises(mc683xx.BusError):
        cpu.run_limited(reach=RETURN)
    cpu2 = machine(MOVE_WORD_A0_TO_D0, RTS)
    cpu2.write_register("A0", 0x2000)
    cpu2.add_pre_read_hook(lambda read, data: cpu2.peek_bytes(0x2000, 2), 0x2000, 0x2001)
    with pytest.raises(RuntimeError):
        cpu2.run_limited(reach=RETURN)


def test_a_true_pre_read_hook_stops_the_run_and_one_that_resizes_raises():
    cpu = machine(MOVE_WORD_A0_TO_D0, NOP, RTS)
    cpu.write_register("A0", 0x2000)
    hook = cpu.add_pre_read_hook(lambda read, data: True, 0x2000, 0x2001)
    run = cpu.run_limited(reach=RETURN)
    assert run.end == mc683xx.End.Stop(mc683xx.Stop.Hook(hook))
    assert cpu.read_register("PC") == CODE + 2
    cpu.remove_hook(hook)
    cpu.write_register("PC", CODE)
    cpu.add_pre_read_hook(lambda read, data: data.append(0), 0x2000, 0x2001)
    with pytest.raises(ValueError, match="length"):
        cpu.run_limited(reach=RETURN)


def test_a_pre_write_hook_replaces_the_long_before_the_write_hook_sees_it():
    cpu = machine(MOVE_LONG_D0_TO_A0, RTS)
    cpu.write_register("A0", 0x2000)
    cpu.write_register("D0", 0x12345678)
    cpu.poke_bytes(0x2000, struct.pack(">I", 0x11112222))
    seen = []
    kept = []

    def replace(write: mc683xx.PendingWrite, data: bytearray) -> None:
        seen.append((write.address, write.size, bytes(data), write.peek(0x2000, 4), write.dropped))
        data[3] = 0x99
        kept.append(write)

    cpu.add_pre_write_hook(replace, 0x2003, 0x2003)
    writes = []
    cpu.add_write_hook(lambda access: writes.append((access.size, access.value, access.previous)), 0x2000, 0x2003)
    assert cpu.run_limited(reach=RETURN).end == mc683xx.End.Reached()
    assert seen == [(0x2000, 4, bytes.fromhex("12345678"), bytes.fromhex("11112222"), False)]
    assert writes == [(4, 0x12345699, 0x11112222)]
    assert cpu.peek_bytes(0x2000, 4) == bytes.fromhex("12345699")
    with pytest.raises(RuntimeError, match="within its hook's call"):
        kept[0].set_dropped(True)
    with pytest.raises(RuntimeError, match="within its hook's call"):
        kept[0].peek(0x2000, 1)


def test_a_dropped_store_writes_nothing_and_a_later_hook_sees_the_drop():
    cpu = machine(MOVE_WORD_D0_TO_A0, RTS)
    cpu.write_register("A0", 0x2000)
    cpu.write_register("D0", 0x1234)
    cpu.poke_bytes(0x2000, b"\x55\x66")
    later = []
    cpu.add_pre_write_hook(lambda write, data: write.set_dropped(True), 0x2000, 0x2001)
    cpu.add_pre_write_hook(lambda write, data: later.append(write.dropped), 0x2000, 0x2001)
    writes = []
    cpu.add_write_hook(writes.append, 0x2000, 0x2001)
    assert cpu.run_limited(reach=RETURN).end == mc683xx.End.Reached()
    assert later == [True] and writes == []
    assert cpu.peek_bytes(0x2000, 2) == b"\x55\x66"


def test_a_true_pre_write_hook_stops_the_run_and_one_that_raises_drops_nothing():
    cpu = machine(MOVE_WORD_D0_TO_A0, NOP, RTS)
    cpu.write_register("A0", 0x2000)
    cpu.write_register("D0", 0x1234)
    hook = cpu.add_pre_write_hook(lambda write, data: True, 0x2000, 0x2001)
    run = cpu.run_limited(reach=RETURN)
    assert run.end == mc683xx.End.Stop(mc683xx.Stop.Hook(hook))
    assert cpu.read_register("PC") == CODE + 2
    assert cpu.peek_bytes(0x2000, 2) == b"\x12\x34"
    cpu.remove_hook(hook)
    cpu.write_register("PC", CODE)
    cpu.write_register("D0", 0x5678)

    def fails(write: mc683xx.PendingWrite, data: bytearray) -> None:
        write.set_dropped(True)
        raise KeyError("model")

    cpu.add_pre_write_hook(fails, 0x2000, 0x2001)
    with pytest.raises(KeyError, match="model"):
        cpu.run_limited(reach=RETURN)
    assert cpu.peek_bytes(0x2000, 2) == b"\x56\x78"


def test_a_memory_hook_sees_a_long_once_and_not_one_that_faults():
    # MOVE.L D0,-(SP); MOVE.L (SP)+,D1; MOVE.L D0,(A0) with A0 at RAM's
    # last word, so its second word is a bus error.
    cpu = machine(0x2F00, 0x221F, MOVE_LONG_D0_TO_A0, RTS)
    cpu.set_stop_on_fault(True)
    cpu.write_register("D0", 0x12345678)
    cpu.write_register("A0", RAM_BYTES - 2)
    cpu.poke_bytes(STACK - 8, struct.pack(">I", 0xCAFEF00D))
    seen = []
    for kind, add in (("read", cpu.add_read_hook), ("write", cpu.add_write_hook)):
        add(
            lambda access, kind=kind: seen.append(
                (kind, access.address, access.size, access.value, access.previous)
            )
        )
    run = cpu.run_limited(reach=RETURN)
    assert isinstance(run.end, mc683xx.End.Fault)
    # A write sees what it replaced; a read, nothing.
    assert seen == [
        ("write", STACK - 8, 4, 0x12345678, 0xCAFEF00D),
        ("read", STACK - 8, 4, 0x12345678, None),
    ]


def test_a_true_hook_or_stop_ends_the_run():
    cpu = machine(NOP, NOP, RTS)
    hook = cpu.add_code_hook(lambda instruction: True, CODE + 2, CODE + 2)
    run = cpu.run_limited(reach=RETURN)
    assert run.end == mc683xx.End.Stop(mc683xx.Stop.Hook(hook))
    assert cpu.read_register("PC") == CODE + 2
    assert cpu.remove_hook(hook)
    stop = cpu.stop_request()
    cpu.add_code_hook(lambda instruction: stop.request(), CODE + 4, CODE + 4)
    assert cpu.run_limited(reach=RETURN).end == mc683xx.End.Stop(mc683xx.Stop.Requested())
    assert cpu.read_register("PC") == CODE + 4


def test_a_stop_requested_between_runs_is_withdrawn():
    cpu = machine(NOP, NOP, RTS)
    cpu.stop_request().request()
    assert cpu.run_limited(reach=RETURN).end == mc683xx.End.Reached()


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
        cpu.run_limited(reach=RETURN)


def test_an_exception_hook_may_return_from_the_handler():
    cpu = machine(trap(3), RTS)
    entries = []

    def entered(entry):
        entries.append(entry.vector)
        # rte by hand: pop the four-word frame (CPU32RM 6.4).
        sp = cpu.read_register("SSP")
        sr, pc = struct.unpack(">HI", cpu.peek_bytes(sp, 6))
        cpu.write_register("SSP", sp + 8)
        cpu.write_register("SR", sr)
        cpu.write_register("PC", pc)

    cpu.add_exception_hook(entered)
    assert cpu.run_limited(reach=RETURN).end == mc683xx.End.Reached()
    assert entries == [TRAP_0 + 3]


def test_an_exception_return_hook_sees_rte_pop_the_frame():
    cpu = machine(trap(3), RTS)
    cpu.poke_bytes(HANDLER, words(RTE))
    entries, returns = [], []
    cpu.add_exception_hook(lambda entry: entries.append(entry.stack))
    hook = cpu.add_exception_return_hook(lambda back: returns.append(back) or True)
    run = cpu.run_limited(reach=RETURN)
    # Stopped after the RTE, before the instruction it returns to.
    assert run.end == mc683xx.End.Stop(mc683xx.Stop.Hook(hook))
    assert cpu.read_register("PC") == CODE + 2
    (back,) = returns
    assert (back.vector, back.pc, back.sr, back.stack) == (TRAP_0 + 3, CODE + 2, SUPERVISOR, entries[0])
    assert cpu.read_register("SSP") == back.stack + 8
    assert cpu.run_limited(reach=RETURN).end == mc683xx.End.Reached()


def test_an_intercept_handles_or_declines_before_stacking():
    cpu = machine(trap(1), trap(7), RTS)
    seen = []

    def answer(intercepted):
        seen.append((intercepted.vector, intercepted.pc))
        if intercepted.vector == TRAP_0 + 1:
            cpu.write_register("PC", intercepted.pc + 2)
            return True
        return False

    hook = cpu.add_intercept_hook(answer, [TRAP_0 + 1, TRAP_0 + 7])
    assert isinstance(hook, mc683xx.HookId)
    run = cpu.run_limited(reach=HANDLER)
    assert run.end == mc683xx.End.Reached()
    assert seen == [(TRAP_0 + 1, CODE), (TRAP_0 + 7, CODE + 2)]
    # Declined, trap #7 was taken natively: its frame is stacked.
    assert cpu.read_register("SSP") == STACK - 4 - 8
    assert cpu.remove_hook(hook)
    assert not cpu.remove_hook(hook)


def test_intercept_hooks_answer_in_order_until_one_handles():
    cpu = machine(trap(1), trap(1), RTS)
    called = []

    def hook(number, handles):
        def answer(intercepted):
            called.append(number)
            if handles:
                cpu.write_register("PC", intercepted.pc + 2)
            return handles

        return answer

    cpu.add_intercept_hook(hook(1, False), [TRAP_0 + 1])
    cpu.add_intercept_hook(hook(2, False), [TRAP_0 + 2])
    handling = cpu.add_intercept_hook(hook(3, True), [TRAP_0 + 1])
    cpu.add_intercept_hook(hook(4, True), [TRAP_0 + 1])
    # a handled trap is no instruction run: both answered, then RTS
    assert cpu.run_limited(instructions=1).end == mc683xx.End.Instructions()
    assert called == [1, 3, 1, 3]
    assert cpu.read_register("PC") == RETURN
    # removed, the next covering hook answers
    cpu.remove_hook(handling)
    called.clear()
    cpu.write_register("A7", STACK - 4)
    cpu.run_limited(begin=CODE, instructions=1)
    assert called == [1, 4, 1, 4]
    with pytest.raises(TypeError):
        cpu.add_intercept_hook(42, [TRAP_0])


def test_an_exception_no_hook_handles_is_taken_natively():
    cpu = machine(trap(1), RTS)
    cpu.add_intercept_hook(lambda intercepted: False, [TRAP_0 + 1])
    assert cpu.run_limited(reach=HANDLER).end == mc683xx.End.Reached()
    assert cpu.read_register("SSP") == STACK - 4 - 8


def test_an_exception_stop_stacks_nothing_and_the_run_on_takes_it():
    cpu = machine(trap(1), RTS)
    cpu.set_stop_on_exception(None)
    assert cpu.stops_on_exception == mc683xx.VectorSet.ALL
    match cpu.run_limited(reach=HANDLER).end:
        case mc683xx.End.Exception(vector, pc):
            assert (vector, pc) == (TRAP_0 + 1, CODE)
        case end:
            raise AssertionError(end)
    end = mc683xx.End.Exception(TRAP_0 + 1, CODE)
    assert (str(end), repr(end)) == ("exception", f"End.Exception({TRAP_0 + 1}, {CODE:#010x})")
    assert cpu.read_register("SSP") == STACK - 4
    assert cpu.run_limited(reach=HANDLER).end == mc683xx.End.Reached()
    assert cpu.read_register("SSP") == STACK - 4 - 8


def test_an_interrupt_stops_once_acknowledged():
    cpu = machine(NOP, RTS)
    cpu.set_stop_on_exception(None)
    cpu.request_interrupt(3, 0x40)
    assert cpu.run_limited(reach=RETURN).end == mc683xx.End.Exception(0x40, CODE)
    assert cpu.run_limited(reach=HANDLER).end == mc683xx.End.Reached()
    cpu.set_stop_on_exception(set())
    assert cpu.stops_on_exception == mc683xx.VectorSet.NONE


def test_an_exception_stop_stops_only_the_vectors_set():
    cpu = machine(trap(1), RTS)
    cpu.set_stop_on_exception({TRAP_0 + 2})
    assert cpu.stops_on_exception == mc683xx.VectorSet([TRAP_0 + 2])
    assert set(cpu.stops_on_exception) == {TRAP_0 + 2}
    assert cpu.run_limited(reach=HANDLER).end == mc683xx.End.Reached()
    cpu = machine(trap(1), RTS)
    cpu.set_stop_on_exception(vector for vector in [TRAP_0 + 1])
    assert cpu.run_limited(reach=HANDLER).end == mc683xx.End.Exception(TRAP_0 + 1, CODE)
    with pytest.raises(TypeError):
        cpu.set_stop_on_exception(True)


def test_an_intercept_hook_on_every_vector_sees_any():
    cpu = machine(trap(1), RTS)
    seen: list[int] = []
    cpu.add_intercept_hook(lambda intercepted: seen.append(intercepted.vector))
    assert cpu.run_limited(reach=HANDLER).end == mc683xx.End.Reached()
    assert seen == [TRAP_0 + 1]


def test_a_fault_stops_the_run_with_nothing_stacked():
    cpu = machine(moveq(3, 1), MOVE_LONG_A0_TO_D0, RTS)
    cpu.set_stop_on_fault(True)
    cpu.write_register("A0", 0x90000000)
    match cpu.run_limited(reach=RETURN).end:
        case mc683xx.End.Fault(fault):
            pass
        case end:
            raise AssertionError(end)
    assert (fault.kind, fault.cause) == (mc683xx.BusCycleKind.READ, mc683xx.FaultCause.BUS_ERROR)
    assert (fault.address, fault.size, fault.pc) == (0x90000000, 4, CODE + 2)
    assert fault.registers["D1"] == 3
    # str() is Rust's Display, the address and PC in full
    assert str(fault) == "bus error: read of 4 bytes at 0x90000000 in supervisor-data, pc 0x31400002"
    assert str(mc683xx.End.Fault(fault)) == "fault"
    assert cpu.read_register("PC") == CODE + 2
    assert cpu.read_register("SSP") == STACK - 4


def test_a_write_to_read_only_code_is_protected():
    cpu = machine(MOVE_WORD_D0_TO_A0, RTS)
    cpu.set_stop_on_fault(True)
    cpu.write_register("A0", CODE)
    end = cpu.run_limited(reach=RETURN).end
    assert isinstance(end, mc683xx.End.Fault)
    fault = end.fault
    assert (fault.kind, fault.cause) == (mc683xx.BusCycleKind.WRITE, mc683xx.FaultCause.PROTECTED)
    assert end == mc683xx.End.Fault(fault=fault)


class Register(mc683xx.MappedPeripheral):
    """A peripheral, typed by the protocol it subclasses: a long
    register, and a log of its cycles."""

    def __init__(self) -> None:
        self.value = 0x11223344
        self.cycles: list[tuple[str, int, int]] = []

    def read(self, cycle: mc683xx.BusCycle, /) -> int:
        self.cycles.append(("read", cycle.address, cycle.size))
        return self.value if cycle.size == 4 else self.value & 0xFFFF

    def write(self, cycle: mc683xx.BusCycle, value: int, /) -> None:
        self.cycles.append(("write", cycle.address, cycle.size))
        self.value = value


def test_a_peripheral_takes_a_long_whole_before_it_lands():
    cpu = machine(MOVE_LONG_A0_TO_D0, ADDQ_LONG_1_D0, MOVE_LONG_D0_TO_A0, RTS)
    register = Register()
    cpu.add_peripheral(PERIPHERAL, PERIPHERAL + 3, register)
    cpu.write_register("A0", PERIPHERAL)
    cpu.run_limited(reach=RETURN)
    assert register.cycles == [("read", PERIPHERAL, 4), ("write", PERIPHERAL, 4)]
    assert register.value == 0x11223345


def test_a_peripheral_removed_answers_no_more():
    cpu = machine(MOVE_LONG_A0_TO_D0, RTS)
    number = cpu.add_peripheral(PERIPHERAL, PERIPHERAL + 3, Register())
    assert isinstance(number, mc683xx.PeripheralId)
    shown = str(number).removeprefix("peripheral ")
    assert int(shown) >= 0 and repr(number) == f"PeripheralId({shown})"
    with pytest.raises(TypeError):
        cpu.remove_peripheral(0)
    assert cpu.remove_peripheral(number)
    assert not cpu.remove_peripheral(number)
    # the space is free again
    assert cpu.add_peripheral(PERIPHERAL, PERIPHERAL + 3, Register()) != number


class Window:
    """Bytes a debugger peeks and pokes with no cycle; `peek` declines
    once `closed`."""

    def __init__(self):
        self.held = bytearray(4)
        self.closed = False
        self.calls = []
        self.codes = []

    def read(self, cycle):
        raise mc683xx.BusError()

    def write(self, cycle, value):
        raise mc683xx.BusError()

    def peek(self, cycle, length):
        self.calls.append((cycle.address, length))
        self.codes.append(cycle.function_code)
        at = cycle.address - PERIPHERAL
        return None if self.closed else bytes(self.held[at : at + length])

    def poke(self, cycle, data):
        self.calls.append((cycle.address, len(data)))
        self.codes.append(cycle.function_code)
        if not self.closed:
            at = cycle.address - PERIPHERAL
            self.held[at : at + len(data)] = data
        return not self.closed


def test_peek_bytes_and_poke_bytes_peek_and_poke_a_peripheral():
    cpu = machine(RTS)
    window = Window()
    cpu.add_peripheral(PERIPHERAL, PERIPHERAL + 3, window)
    cpu.poke_bytes(PERIPHERAL, b"\x01\x02\x03\x04")
    assert window.held == b"\x01\x02\x03\x04"
    assert cpu.peek_bytes(PERIPHERAL + 1, 2) == b"\x02\x03"
    assert window.calls == [(PERIPHERAL, 4), (PERIPHERAL + 1, 2)]
    cpu.poke_bytes(PERIPHERAL, b"\x05", mc683xx.FunctionCode.USER_DATA)
    assert cpu.peek_bytes(PERIPHERAL, 1, mc683xx.FunctionCode.USER_PROGRAM) == b"\x05"
    assert window.codes == [
        mc683xx.FunctionCode.SUPERVISOR_DATA,
        mc683xx.FunctionCode.SUPERVISOR_DATA,
        mc683xx.FunctionCode.USER_DATA,
        mc683xx.FunctionCode.USER_PROGRAM,
    ]
    window.closed = True
    with pytest.raises(mc683xx.UnpeekableError, match="not peekable at 0x00f00000") as raised:
        cpu.peek_bytes(PERIPHERAL, 1)
    assert raised.value.address == PERIPHERAL
    with pytest.raises(mc683xx.UnpeekableError):
        cpu.poke_bytes(PERIPHERAL, b"\x00")
    with pytest.raises(mc683xx.BusError):
        cpu.peek_bytes(PERIPHERAL + 4, 1)


def test_a_peek_of_another_length_raises_value_error():
    class Short(Window):
        def peek(self, cycle, length):
            return b"\x00"

    cpu = machine(RTS)
    cpu.add_peripheral(PERIPHERAL, PERIPHERAL + 3, Short())
    with pytest.raises(ValueError, match="a peek of 2 bytes gave 1"):
        cpu.peek_bytes(PERIPHERAL, 2)
    # raised already: no stop is left for the next run
    cpu.write_register("PC", CODE)
    assert cpu.run_limited(reach=RETURN).end == mc683xx.End.Reached()


def test_a_large_write_into_a_peripheral_is_one_call():
    class Array:
        def __init__(self, size):
            self.held = bytearray(size)
            self.calls = 0

        def read(self, cycle):
            raise mc683xx.BusError()

        def write(self, cycle, value):
            raise mc683xx.BusError()

        def peek(self, cycle, length):
            self.calls += 1
            at = cycle.address - PERIPHERAL
            return bytes(self.held[at : at + length])

        def poke(self, cycle, data):
            self.calls += 1
            at = cycle.address - PERIPHERAL
            self.held[at : at + len(data)] = data
            return True

    size = 512 << 10
    cpu = mc683xx.Machine()
    array = Array(size)
    cpu.add_peripheral(PERIPHERAL, PERIPHERAL + size - 1, array)
    image = bytes(range(256)) * (size // 256)
    began = time.perf_counter()
    cpu.poke_bytes(PERIPHERAL, image)
    assert cpu.peek_bytes(PERIPHERAL, size) == image
    assert array.calls == 2
    # one call each way, not one a byte: well under the 3 s a byte at a time took
    assert time.perf_counter() - began < 1.0


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
    assert isinstance(cpu.run_limited(reach=RETURN).end, mc683xx.End.Fault)
    cpu.write_register("PC", CODE + 2)
    with pytest.raises(KeyError):
        cpu.run_limited(reach=RETURN)


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
    cpu.poke_bytes(64 * 4, struct.pack(">I", HANDLER))
    cpu.poke_bytes(HANDLER, words(NOP))
    cpu.request_interrupt(5, 64)
    assert cpu.run_limited(reach=HANDLER).end == mc683xx.End.Reached()
    assert cpu.read_register("SR") >> 8 & 7 == 5
    cpu.clear_interrupt(5)
    with pytest.raises(ValueError):
        cpu.request_interrupt(0)


def test_a_host_exception_is_stacked_and_entered_now():
    cpu = machine(NOP, RTS)
    entries = []
    cpu.add_exception_hook(lambda entry: entries.append((entry.vector, entry.level)) or True)
    cpu.write_register("SR", SUPERVISOR | 0x0700)
    cpu.take_exception(64, level=3)
    # CPU32RM 6.2.11: the mask takes the level; 6.4.1: SR, PC, format $0
    assert cpu.read_register("SR") == SUPERVISOR | 0x0300
    assert cpu.read_register("PC") == HANDLER
    sp = cpu.read_register("SSP")
    assert sp == STACK - 4 - 8
    assert struct.unpack(">HIH", cpu.peek_bytes(sp, 8)) == (SUPERVISOR | 0x0700, CODE, 64 * 4)
    assert entries == [(64, 3)]
    assert cpu.now == 0
    with pytest.raises(ValueError):
        cpu.take_exception(64, level=8)


def test_a_host_exception_refused():
    cpu = machine(NOP, RTS)
    cpu.set_stop_on_fault(True)
    cpu.write_register("SSP", 0x90000000)
    with pytest.raises(mc683xx.RunError, match="stopped at a fault: bus error: write"):
        cpu.take_exception(TRAP_0)
    assert (cpu.read_register("SSP"), cpu.read_register("PC")) == (0x90000000, CODE)
    halted = mc683xx.Machine()
    halted.reset()
    with pytest.raises(mc683xx.RunError, match="executes nothing: halted"):
        halted.take_exception(TRAP_0)


def test_save_and_restore():
    cpu = machine(moveq(1, 0), RTS)
    snapshot = cpu.save()
    cpu.run_limited(reach=RETURN)
    cpu.poke_bytes(0x100, b"\x01")
    cpu.restore(snapshot)
    assert cpu.read_register("PC") == CODE
    assert cpu.peek_bytes(0x100, 1) == b"\x00"
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
    cpu.run_limited(reach=RETURN)
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
        cpu.poke_bytes(STACK - 4, frame)
        cpu.write_register("A7", STACK - 4)
        cpu.run_limited(begin=CODE, reach=RETURN)
    rate = calls / (time.perf_counter() - start)
    print(f"{rate:.0f} calls per second")
    assert rate > 1000


@pytest.mark.parametrize("result", [1, 0, "", [], "yes"])
def test_a_hook_result_not_a_bool_or_none_raises_type_error(result):
    cpu = machine(NOP, NOP, RTS)
    cpu.add_code_hook(lambda instruction: result, CODE + 2, CODE + 2)
    with pytest.raises(TypeError, match="True, False or None"):
        cpu.run_limited(reach=RETURN)


@pytest.mark.parametrize("result", [1, 2.0])
def test_an_intercept_or_pre_access_hook_result_not_a_bool_raises_type_error(result):
    cpu = machine(0x4E40, RTS)  # TRAP #0
    cpu.add_intercept_hook(lambda intercepted: result)
    with pytest.raises(TypeError, match="True, False or None"):
        cpu.run_limited(reach=RETURN)
    cpu = machine(MOVE_WORD_A0_TO_D0, RTS)
    cpu.write_register("A0", 0x2000)
    cpu.add_pre_read_hook(lambda read, data: result, 0x2000, 0x2001)
    with pytest.raises(TypeError, match="True, False or None"):
        cpu.run_limited(reach=RETURN)
    cpu = machine(MOVE_WORD_D0_TO_A0, RTS)
    cpu.write_register("A0", 0x2000)
    cpu.add_pre_write_hook(lambda write, data: result, 0x2000, 0x2001)
    with pytest.raises(TypeError, match="True, False or None"):
        cpu.run_limited(reach=RETURN)


def test_false_and_none_do_not_stop_a_hook():
    cpu = machine(NOP, NOP, RTS)
    cpu.add_code_hook(lambda instruction: False, CODE + 2, CODE + 2)
    cpu.add_code_hook(lambda instruction: None, CODE + 2, CODE + 2)
    assert cpu.run_limited(reach=RETURN).end == mc683xx.End.Reached()


def test_a_vector_set_is_rust_s():
    VectorSet = mc683xx.VectorSet
    traps = VectorSet(range(32, 48))
    assert VectorSet() == VectorSet.NONE and VectorSet.NONE.is_empty()
    assert len(VectorSet.ALL) == 256 and list(VectorSet([39, 32])) == [32, 39]
    assert 32 in traps and 31 not in traps and 300 not in traps and -1 not in traps and "x" not in traps
    assert traps.contains(47) and not traps.without(47).contains(47)
    assert traps.union(VectorSet([4])) == VectorSet([4, *range(32, 48)])
    assert traps.union([4]) == traps.union(VectorSet([4]))
    assert traps.difference(VectorSet(range(33, 48))) == VectorSet([32])
    two = [mc683xx.Vector.trap(1), mc683xx.Vector.trap(7)]
    rest = VectorSet.INSTRUCTION.difference(two)
    assert len(rest) == len(VectorSet.INSTRUCTION) - 2 and 33 not in rest and 39 not in rest
    assert rest == VectorSet.INSTRUCTION.difference(VectorSet(two))
    assert list(VectorSet.INSTRUCTION) == [4, 5, 6, 7, 8, 10, 11, 14, *range(32, 48)]
    assert VectorSet(VectorSet.ALL) == VectorSet.ALL
    assert repr(VectorSet.ALL) == "VectorSet.ALL"
    assert repr(VectorSet([32, 39])) == "VectorSet([32, 39])"
    assert len({VectorSet([1]), VectorSet([1])}) == 1
    with pytest.raises(OverflowError):
        VectorSet([256])
    with pytest.raises(TypeError):
        traps.difference(7)  # type: ignore[arg-type]


def test_a_vector_set_is_taken_wherever_vectors_are():
    cpu = machine(trap(1), RTS)
    cpu.set_stop_on_exception(mc683xx.VectorSet.INSTRUCTION.without(TRAP_0 + 2))
    assert cpu.run_limited(reach=HANDLER).end == mc683xx.End.Exception(TRAP_0 + 1, CODE)
    cpu.set_stop_on_exception(mc683xx.VectorSet.NONE)
    seen = []
    cpu.add_intercept_hook(lambda intercepted: seen.append(intercepted.vector), mc683xx.VectorSet.INSTRUCTION)
    assert cpu.run_limited(reach=HANDLER).end == mc683xx.End.Reached()
    assert seen == [TRAP_0 + 1]


def test_vector_names_rust_s_vector_numbers():
    Vector = mc683xx.Vector
    assert (Vector.BUS_ERROR, Vector.ZERO_DIVIDE, Vector.TRAP, Vector.FORMAT_ERROR) == (2, 5, 32, 14)
    assert Vector.trap(7) == 39 and Vector.autovector(3) == 27
    assert Vector.TRAP + 1 in mc683xx.VectorSet.INSTRUCTION
    with pytest.raises(ValueError, match="TRAP #16"):
        Vector.trap(16)
    with pytest.raises(ValueError):
        Vector.autovector(8)
    with pytest.raises(TypeError):
        Vector()


BRA_SELF = 0x60FE  # BRA.S to itself


def test_a_limited_run_can_end_at_a_self_loop_once_it_runs():
    cpu = machine(NOP, BRA_SELF)
    run = cpu.run_limited(self_loop=True, instructions=100)
    assert run.end == mc683xx.End.SelfLoop(CODE + 2)
    assert isinstance(run.end, mc683xx.End.SelfLoop) and run.end.pc == CODE + 2
    assert repr(run.end) == f"End.SelfLoop({CODE + 2:#010x})"
    assert str(run.end) == "self-loop"
    assert cpu.instructions == 2
    # not asked: the loop runs on
    assert cpu.run_limited(instructions=10).end == mc683xx.End.Instructions()
    assert cpu.instructions == 12


def test_a_coverage_records_the_instructions_run_in_its_range():
    cpu = machine(NOP, ADDQ_LONG_1_D0, RTS)
    coverage = cpu.add_coverage(CODE, CODE + 0xFF)
    assert repr(coverage) == "CoverageId(1)" and str(coverage) == "coverage 1"
    cpu.run_limited(reach=RETURN)
    assert cpu.coverage(coverage) == {CODE, CODE + 2, CODE + 4}
    assert cpu.clear_coverage(coverage)
    assert cpu.coverage(coverage) == set()
    cpu.poke_bytes(STACK - 4, struct.pack(">I", RETURN))
    cpu.write_register("A7", STACK - 4)
    cpu.run_limited(begin=CODE + 2, reach=RETURN)
    # not saved: a restore leaves it as it was
    cpu.restore(cpu.save())
    assert cpu.remove_coverage(coverage) == {CODE + 2, CODE + 4}
    assert cpu.coverage(coverage) is None
    assert not cpu.clear_coverage(coverage)
    assert cpu.remove_coverage(coverage) is None
