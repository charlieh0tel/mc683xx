"""The chip from Python: a ROM on CSBOOT, RAM on CS0, a Python peripheral
on CS1, and small programs that reach them."""

import struct

import pytest

import mc683xx

# The SIM's registers with MM set (MC68331UM D.3).
SIM = 0xFFFA00
# CSBAR0 and CSOR0; CSBAR1 and CSOR1 (MC68331UM Table D-3).
CSBAR0, CSOR0, CSBAR1, CSOR1 = SIM + 0x4C, SIM + 0x4E, SIM + 0x50, SIM + 0x52
# CSOR: both bytes, read and write, fast termination, supervisor and
# user space (MC68331UM D.3.26).
CSOR_FAST = 0x7BB0
# CSBAR's base field holds ADDR[23:11] from bit 3; BLKSZ 011 is 64
# Kbytes (MC68331UM D.3.25, Table 4-21).
BLOCK_64K = 0b011
RAM_BASE = 0x100000
PERIPHERAL_BASE = 0x200000

ROM_BYTES = 64 << 10
RAM_BYTES = 64 << 10
STACK = RAM_BASE + RAM_BYTES
START = 0x400
# Where the bus error and level 7 autovector handlers start.
BUS_ERROR_HANDLER = 0x800
LEVEL_7_HANDLER = 0x900
# Vector numbers (CPU32RM Table 6-1).
BUS_ERROR = 2
LEVEL_7_AUTOVECTOR = 31

# Opcodes (CPU32RM 4).
NOP = 0x4E71
MOVEQ = 0x7000
MOVE_WORD_IMMEDIATE_TO_ABSOLUTE = 0x33FC
MOVE_WORD_D0_TO_ABSOLUTE = 0x33C0
MOVE_WORD_ABSOLUTE_TO_D1 = 0x3239


def csbar(base):
    return base >> 8 | BLOCK_64K


def store_word(address, value):
    return [MOVE_WORD_IMMEDIATE_TO_ABSOLUTE, value, address >> 16, address & 0xFFFF]


# RAM on CS0 and the peripheral on CS1.
WIRING = (
    store_word(CSBAR0, csbar(RAM_BASE))
    + store_word(CSOR0, CSOR_FAST)
    + store_word(CSBAR1, csbar(PERIPHERAL_BASE))
    + store_word(CSOR1, CSOR_FAST)
)


def rom(program):
    """A ROM of NOPs: the reset vectors, the bus error and level 7
    autovectors, and `program` at START."""
    image = bytearray(struct.pack(">H", NOP) * (ROM_BYTES // 2))
    struct.pack_into(">II", image, 0, STACK, START)
    struct.pack_into(">I", image, 4 * BUS_ERROR, BUS_ERROR_HANDLER)
    struct.pack_into(">I", image, 4 * LEVEL_7_AUTOVECTOR, LEVEL_7_HANDLER)
    struct.pack_into(f">{len(program)}H", image, START, *program)
    return bytes(image)


class Recorder:
    """Records its cycles; reads as `value`."""

    def __init__(self, value=0):
        self.value = value
        self.reads = []
        self.writes = []
        self.resets = []

    def read(self, cycle):
        self.reads.append((cycle.select, cycle.address, cycle.size))
        return self.value

    def write(self, cycle, value):
        self.writes.append((cycle.select, cycle.address, cycle.size, value))

    def reset(self, now):
        self.resets.append(now)


class Register:
    """Reads as the last word written, which it saves."""

    def __init__(self):
        self.value = 0

    def read(self, cycle):
        return self.value

    def write(self, cycle, value):
        self.value = value

    def save(self):
        return struct.pack(">H", self.value)

    def restore(self, snapshot):
        (self.value,) = struct.unpack(">H", snapshot)


class Fixed:
    """A model with no state: an empty snapshot."""

    def save(self):
        return b""

    def restore(self, snapshot):
        if snapshot:
            raise ValueError("not a Fixed snapshot")


class Faulting(Recorder):
    def read(self, cycle):
        raise mc683xx.BusError()


class Broken(Recorder):
    def read(self, cycle):
        return 1 // 0


class Alarm:
    """A model whose interrupt output asserts at `at`."""

    def __init__(self, at):
        self.at = at
        self.now = 0
        self.advanced = []

    def next_event(self):
        return self.at if self.now < self.at else None

    def advance(self, now):
        self.now = now
        self.advanced.append(now)

    def interrupt_request(self):
        return self.now >= self.at


def board(program, peripheral):
    builder = mc683xx.Builder()
    builder.memory(rom(program), ["CSBOOT"], writable=False)
    ram = builder.memory(bytes(RAM_BYTES), ["CS0"])
    number = builder.peripheral(peripheral, ["CS1"])
    return builder, ram, number


def test_a_program_reaches_the_peripheral():
    program = WIRING + [
        MOVEQ | 0x42,
        MOVE_WORD_D0_TO_ABSOLUTE, PERIPHERAL_BASE >> 16, 0x0010,
        MOVE_WORD_ABSOLUTE_TO_D1, PERIPHERAL_BASE >> 16, 0x0020,
    ]
    peripheral = Recorder(0xBEEF)
    builder, _, number = board(program, peripheral)
    chip = builder.build()
    assert chip.read_register("PC") == START
    assert chip.read_register("SSP") == STACK
    assert chip.peripheral(number) is peripheral
    assert peripheral.resets == [0]
    run = chip.run_for(2000)
    assert run.stop == mc683xx.Stop.Budget()
    assert run.cycles >= 2000
    assert chip.now == run.cycles
    assert peripheral.writes == [("CS1", PERIPHERAL_BASE + 0x10, 2, 0x42)]
    assert peripheral.reads == [("CS1", PERIPHERAL_BASE + 0x20, 2)]
    assert chip.read_register("D1") == 0xBEEF
    assert chip.registers()["D0"] == 0x42


def test_bus_error_from_a_peripheral_takes_the_exception():
    program = WIRING + [MOVE_WORD_ABSOLUTE_TO_D1, PERIPHERAL_BASE >> 16, 0x0020]
    builder, ram, _ = board(program, Faulting())
    chip = builder.build()
    chip.run_for(2000)
    assert chip.halted is None
    assert chip.read_register("PC") > BUS_ERROR_HANDLER
    # a format $C frame: its format and vector word at SP + 6 (CPU32RM
    # 6.4.3)
    sp = chip.read_register("A7")
    (word,) = struct.unpack(">H", chip.read_memory(ram, sp - RAM_BASE + 6, 2))
    assert word == 0xC000 | 4 * BUS_ERROR


def test_a_peripheral_exception_is_raised_by_run():
    program = WIRING + [MOVE_WORD_ABSOLUTE_TO_D1, PERIPHERAL_BASE >> 16, 0x0020]
    builder, _, _ = board(program, Broken())
    chip = builder.build()
    chip.write_register("D1", 0xFFFF)
    with pytest.raises(ZeroDivisionError):
        chip.run_for(2000)
    # The run stopped once the instruction ended, the bus not faulted:
    # the read undriven, 0, and no bus error taken.
    assert chip.read_register("PC") == START + 2 * len(program)
    assert chip.read_register("D1") == 0
    assert chip.now < 2000
    # once
    run = chip.run_for(100)
    assert run.stop == mc683xx.Stop.Budget()


def test_tracing_sees_peripheral_cycles_and_exceptions():
    program = WIRING + [
        MOVE_WORD_ABSOLUTE_TO_D1, PERIPHERAL_BASE >> 16, 0x0020,
    ]
    builder, _, _ = board(program, Faulting())
    chip = builder.build()
    chip.set_tracing(accesses=True, exceptions=True)
    chip.run_for(2000)
    events = chip.take_events()
    accesses = [e for e in events if isinstance(e, mc683xx.Event.Access)]
    # the four chip-select writes, then the peripheral's bus error
    assert [(e.address, e.write) for e in accesses[:4]] == [
        (CSBAR0, True), (CSOR0, True), (CSBAR1, True), (CSOR1, True),
    ]
    assert (accesses[4].address, accesses[4].bus_error) == (PERIPHERAL_BASE + 0x20, True)
    exceptions = [e for e in events if isinstance(e, mc683xx.Event.Exception)]
    assert exceptions[0].vector == BUS_ERROR
    assert exceptions[0].handler == BUS_ERROR_HANDLER
    assert chip.instructions > 5


def test_a_model_interrupts_on_time():
    builder, _, _ = board(WIRING, Recorder())
    alarm = Alarm(5000)
    number = builder.unwired_peripheral(alarm)
    builder.interrupt(number, 7)
    builder.autovector(7)
    chip = builder.build()
    chip.run_for(4000)
    assert chip.read_register("PC") < LEVEL_7_HANDLER
    chip.run_for(4000)
    # a quantum ends at its event, past it by at most an instruction
    assert [now - 5000 for now in alarm.advanced if 5000 <= now < 5100] == [1]
    assert chip.read_register("PC") > LEVEL_7_HANDLER


def test_the_irq_pins():
    builder, _, _ = board(WIRING, Recorder())
    builder.autovector(7)
    chip = builder.build()
    chip.run_for(1000)
    chip.set_interrupt_pin(7, True)
    chip.run_for(1000)
    assert chip.read_register("PC") > LEVEL_7_HANDLER
    with pytest.raises(ValueError):
        chip.set_interrupt_pin(0, True)


def test_resets_are_events():
    peripheral = Recorder()
    builder, _, _ = board([NOP], peripheral)
    chip = builder.build()
    chip.run_for(100)
    chip.reset()
    events = chip.take_events()
    assert [type(event) for event in events] == [mc683xx.Event.Reset] * 2
    assert (events[0].now, events[0].cause) == (0, mc683xx.ResetCause.POWER_ON)
    assert events[1].cause == mc683xx.ResetCause.EXTERNAL
    assert peripheral.resets == [0, events[1].now]
    assert chip.take_events() == []


def test_registers_and_memory():
    builder, ram, _ = board([NOP], Recorder())
    chip = builder.build()
    chip.write_register("d3", 0xDEADBEEF)
    assert chip.read_register("D3") == 0xDEADBEEF
    chip.write_register("CCR", 0x1F)
    assert chip.read_register("SR") & 0xFF == 0x1F
    with pytest.raises(ValueError):
        chip.write_register("SR", 0x10000)
    with pytest.raises(ValueError):
        chip.read_register("D8")
    chip.write_memory(ram, 0x10, b"\x01\x02")
    assert chip.read_memory(ram, 0x10, 2) == b"\x01\x02"
    assert len(chip.memory(ram)) == RAM_BYTES
    with pytest.raises(IndexError):
        chip.read_memory(ram, RAM_BYTES - 1, 2)
    with pytest.raises(IndexError):
        chip.memory(7)
    assert chip.system_hz == 32768 * 256


def test_wiring_mistakes():
    builder = mc683xx.Builder()
    with pytest.raises(ValueError):
        builder.memory(bytes(100), ["CSBOOT"])
    with pytest.raises(ValueError):
        builder.memory(bytes(2048), ["CS11"])
    builder.memory(bytes(2048), ["CSBOOT"])
    with pytest.raises(ValueError):
        builder.peripheral(Recorder(), ["CSBOOT"])
    with pytest.raises(TypeError):
        builder.peripheral(object(), ["CS2"])
    with pytest.raises(IndexError):
        builder.interrupt(3, 1)
    with pytest.raises(ValueError):
        mc683xx.Builder(reference_hz=32768, external_hz=16_000_000)
    with pytest.raises(ValueError):
        mc683xx.Builder(reference_hz=4_194_304)
    builder.build()
    with pytest.raises(RuntimeError):
        builder.build()


def test_an_external_clock():
    builder = mc683xx.Builder(external_hz=16_000_000)
    builder.memory(rom([NOP]), ["CSBOOT"], writable=False)
    assert builder.build().system_hz == 16_000_000


# The QSM's and GPT's registers with MM set (MC68331UM Table D-1).
QSM = 0xFFFC00
GPT = 0xFFF900
# SCCR0, SCCR1, SCSR, SCDR (QSMRM 5.2).
SCCR0, SCCR1, SCSR, SCDR = QSM + 0x08, QSM + 0x0A, QSM + 0x0C, QSM + 0x0E
# SCCR1 TE and RE; SCSR RDRF (QSMRM 5.2.2, 5.2.3).
SCCR1_TE_RE = 0x000C
SCSR_RDRF = 0x0040
# PORTQS, PQSPAR and DDRQS, SPCR0, SPCR1, receive, transmit and
# command RAM (QSMRM 4.2).
PORTQS, PQSPAR, SPCR0, SPCR1 = QSM + 0x15, QSM + 0x16, QSM + 0x18, QSM + 0x1A
RECEIVE_RAM, TRANSMIT_RAM, COMMAND_RAM = QSM + 0x100, QSM + 0x120, QSM + 0x140
# TCTL1 in the upper byte of its word, TOC2 (GPTRM 3.3).
TCTL1, TOC2 = GPT + 0x1E, GPT + 0x16
# TCTL1 OM2:OL2 01: toggle OC2's pin; OC2 is PGP4 (GPTRM 3.3.2).
TCTL1_OC2_TOGGLE = 0x0100
OC2_PIN = 0x10

MOVE_BYTE_IMMEDIATE_TO_ABSOLUTE = 0x13FC
MOVE_BYTE_D0_TO_ABSOLUTE = 0x13C0
MOVE_WORD_ABSOLUTE_TO_D0 = 0x3039
ANDI_WORD_D1 = 0x0241
BEQ_SHORT = 0x6700
BRA_SHORT = 0x6000
DBRA_D3 = 0x51CB


def store_byte(address, value):
    return [MOVE_BYTE_IMMEDIATE_TO_ABSOLUTE, value, address >> 16, address & 0xFFFF]


def load_word(address, register):
    return [MOVE_WORD_ABSOLUTE_TO_D0 | register << 9, address >> 16, address & 0xFFFF]


def short_branch(opcode, words_back):
    """A short branch to `words_back` words before the word after it."""
    return opcode | (-2 * words_back) & 0xFF


def booting(program):
    builder = mc683xx.Builder()
    builder.memory(rom(program), ["CSBOOT"], writable=False)
    return builder


def test_the_sci_echoes_what_the_host_sends():
    # SCBR 1; TE and RE; then each frame received is sent back
    poll = load_word(SCSR, 1) + [ANDI_WORD_D1, SCSR_RDRF]
    poll += [short_branch(BEQ_SHORT, len(poll) + 1)]
    echo = load_word(SCDR, 0) + [MOVE_BYTE_D0_TO_ABSOLUTE, (SCDR + 1) >> 16, (SCDR + 1) & 0xFFFF]
    loop = poll + echo
    program = store_word(SCCR0, 1) + store_word(SCCR1, SCCR1_TE_RE) + loop
    program += [short_branch(BRA_SHORT, len(loop) + 1)]
    chip = booting(program).build()
    chip.run_for(1000)
    assert chip.take_sci_transmitted() == []
    chip.sci_receive(mc683xx.Frame.Data(0x21))
    chip.run_for(2000)
    sent = chip.take_sci_transmitted()
    assert [frame for _, frame in sent] == [mc683xx.Frame.Data(0x21)]
    assert sent[0][0] <= chip.now
    with pytest.raises(ValueError):
        chip.sci_receive(mc683xx.Frame.Data(0x200))
    chip.sci_receive(mc683xx.Frame.Break())


class Peripheral:
    """A QSPI peripheral answering `value`."""

    def __init__(self, value):
        self.value = value
        self.transfers = []
        self.levels = []
        self.clocks = []
        self.transfer_clocks = []

    def transfer(self, transfer):
        self.transfers.append((transfer.chip_selects, transfer.out, transfer.bits))
        self.transfer_clocks.append(transfer.clock)
        return self.value

    def chip_selects(self, levels, now):
        self.levels.append(levels)

    def clock(self, clock, now):
        self.clocks.append(clock)


def test_a_qspi_transfer_reaches_the_peripheral():
    # PCS[3:0] outputs, high, the QSPI's; one entry to PCS 0101, BITSE clear: 8 bits
    # at SPBR 2; then a wait and RR[0] to D2 (QSMRM 4.3)
    program = (
        store_byte(PORTQS, 0x78)
        + store_word(PQSPAR, 0x7878)
        + store_word(TRANSMIT_RAM, 0x1234)
        + store_byte(COMMAND_RAM, 0b0101)
        + store_word(SPCR0, 0x8002)
        + store_word(SPCR1, 0x8000)
        + [MOVEQ | 3 << 9 | 20, DBRA_D3, 0xFFFE]
        + load_word(RECEIVE_RAM, 2)
    )
    builder = booting(program)
    peripheral = Peripheral(0xEF)
    builder.qspi(peripheral)
    chip = builder.build()
    chip.run_for(5000)
    assert chip.qspi_peripheral() is peripheral
    assert peripheral.transfers == [(0b0101, 0x34, 8)]
    assert 0b0101 in peripheral.levels
    assert chip.read_register("D2") & 0xFFFF == 0xEF
    driven, _ = chip.port_qs_outputs()
    assert driven & 0x78 == 0x78
    assert chip.qspi_chip_selects == peripheral.levels[-1]
    # CPOL and CPHA clear, SPBR 2: an SCK period of 4 clocks (QSMRM 4.3.1)
    [clock] = peripheral.clocks
    assert clock.polarity == mc683xx.ClockPolarity.IDLE_LOW
    assert clock.phase == mc683xx.ClockPhase.CAPTURE_LEADING
    assert (clock.period, clock.system_clock_hz) == (4, chip.system_hz)
    assert clock.baud_hz == chip.system_hz // 4
    [carried] = peripheral.transfer_clocks
    assert (carried.polarity, carried.phase, carried.period) == (
        clock.polarity,
        clock.phase,
        clock.period,
    )
    chip.set_port_qs_pins(0xFF)
    with pytest.raises(TypeError):
        mc683xx.Builder().qspi(object())


class EntryPeripheral:
    """A QSPI peripheral that keeps each transfer's queue entry."""

    def __init__(self):
        self.transfers = []

    def transfer(self, transfer):
        self.transfers.append((transfer.entry, transfer.chip_selects, transfer.out, transfer.bits))
        return 0


def test_a_qspi_peripheral_is_told_the_entry():
    # one entry, 3: NEWQP and ENDQP in SPCR2 (QSMRM 4.2.4)
    spcr2, entry = QSM + 0x1C, 3
    program = (
        store_byte(PORTQS, 0x78)
        + store_word(PQSPAR, 0x7878)
        + store_word(TRANSMIT_RAM + 2 * entry, 0x1234)
        + store_byte(COMMAND_RAM + entry, 0b0101)
        + store_word(spcr2, entry << 8 | entry)
        + store_word(SPCR0, 0x8002)
        + store_word(SPCR1, 0x8000)
    )
    builder = booting(program)
    peripheral = EntryPeripheral()
    builder.qspi(peripheral)
    chip = builder.build()
    chip.run_for(5000)
    assert peripheral.transfers == [(entry, 0b0101, 0x34, 8)]


def test_the_host_masters_the_qspi_as_a_slave():
    # MISO an output, MOSI and PCS0/SS inputs; BITS 8, slave; ENDQP 0
    # (MC68331UM 6.3.5.3)
    program = (
        store_word(PQSPAR, 0x0B01)
        + store_word(TRANSMIT_RAM, 0x00C3)
        + store_word(SPCR0, 0x2000)
        + store_word(SPCR1, 0x8000)
    )
    chip = booting(program).build()
    chip.run_for(1000)
    chip.set_qspi_slave_select(True)
    start = chip.now
    assert chip.qspi_slave_shift(0x5A, 8, 10) == (0xC3, 0xFF)
    assert chip.now >= start + 80
    # the queue ended at ENDQP: nothing more is driven
    assert chip.qspi_slave_shift(0x5A, 8, 10) == (0, 0)
    chip.set_qspi_slave_select(False)
    with pytest.raises(ValueError):
        chip.qspi_slave_shift(0x100, 8, 10)
    with pytest.raises(ValueError):
        chip.qspi_slave_shift(0, 17, 10)


def test_a_gpt_output_compare_toggles_its_pin():
    # OC2 toggles at TCNT $1000; TCNT counts the system clock / 4
    program = store_word(TCTL1, TCTL1_OC2_TOGGLE) + store_word(TOC2, 0x1000)
    chip = booting(program).build()
    chip.run_for(4 * 0x1000 - 1000)
    assert chip.port_gp_outputs() == (OC2_PIN, 0)
    chip.set_report_pwm_edges(mc683xx.Channel.B, True)
    chip.run_for(2000)
    assert chip.port_gp_outputs() == (OC2_PIN, OC2_PIN)
    changes = chip.take_gpt_changes()
    assert [change for _, change in changes] == [
        mc683xx.OutputChange.PortGp(OC2_PIN, 0),
        mc683xx.OutputChange.PortGp(OC2_PIN, OC2_PIN),
    ]
    assert changes[-1][0] == 4 * 0x1000
    assert chip.take_gpt_changes() == []
    with pytest.raises(TypeError):
        chip.set_report_pwm_edges("B", True)  # type: ignore[arg-type]
    chip.schedule_gpt_input(chip.now + 100, "PAI", False)
    chip.schedule_gpt_input(chip.now + 200, "PCLK", False)
    with pytest.raises(ValueError):
        chip.schedule_gpt_input(0, "PGP8", True)
    chip.run_for(300)
    assert isinstance(chip.pwm(mc683xx.Channel.A), mc683xx.Output)
    assert isinstance(chip.oc1(), mc683xx.Output.Level)
    chip.set_port_gp_pins(0xFF)
    chip.set_pai(True)
    chip.set_pclk(True)


# TMSK2 in the lower byte of its word; CPROUT with CPR %011, TCNT's
# clock, the system clock / 32, on OC1, PGP3 (GPTRM 5.1, 7.1.3.1).
TMSK = GPT + 0x20
TMSK2_CPROUT_CPR_32 = 0x000B
CPR_32_DIVISOR = 32
OC1_PIN = 0x08


def test_cprout_edges_come_at_tcnts_clock():
    chip = booting(store_word(TMSK, TMSK2_CPROUT_CPR_32)).build()
    chip.run_for(1000)
    assert chip.take_gpt_changes()[-1][1] == mc683xx.OutputChange.PortGp(OC1_PIN, 0)
    chip.set_report_oc1_clock_edges(True)
    chip.run_for(10 * CPR_32_DIVISOR)
    changes = chip.take_gpt_changes()
    ports = [(now, change) for now, change in changes if isinstance(change, mc683xx.OutputChange.PortGp)]
    assert len(ports) == len(changes)
    assert {change.driven for _, change in ports} == {OC1_PIN}
    rises = [now for now, change in ports if change.levels]
    falls = [now for now, change in ports if not change.levels]
    assert len(rises) >= 9
    assert all(now % CPR_32_DIVISOR == rises[0] % CPR_32_DIVISOR for now in rises)
    assert falls[: len(rises)] == [now + 1 for now in rises][: len(falls)]
    chip.set_report_oc1_clock_edges(False)


def test_every_rsr_cause_is_a_reset_cause():
    builder, _, _ = board([NOP], Recorder())
    causes = [
        mc683xx.ResetCause.EXTERNAL,
        mc683xx.ResetCause.POWER_ON,
        mc683xx.ResetCause.WATCHDOG,
        mc683xx.ResetCause.HALT_MONITOR,
        mc683xx.ResetCause.SYSTEM,
        mc683xx.ResetCause.LOSS_OF_CLOCK,
        mc683xx.ResetCause.TEST,
    ]
    assert len(set(causes)) == 7
    builder.reset_scope(0, causes)


def test_a_reset_scope_keeps_a_peripheral_out_of_the_watchdog_reset():
    peripheral, kept = Recorder(), Recorder()
    builder, _, _ = board([NOP], peripheral)
    number = builder.unwired_peripheral(kept)
    causes = [
        mc683xx.ResetCause.POWER_ON,
        mc683xx.ResetCause.EXTERNAL,
    ]
    builder.reset_scope(number, causes)
    with pytest.raises(IndexError):
        builder.reset_scope(9, causes)
    chip = builder.build()
    # the watchdog's 2^9 EXTAL periods at 256 clocks
    chip.run_for(512 * 256 + 100)
    chip.reset()
    events = chip.take_events()
    assert [event.cause for event in events] == [
        mc683xx.ResetCause.POWER_ON,
        mc683xx.ResetCause.WATCHDOG,
        mc683xx.ResetCause.EXTERNAL,
    ]
    assert len(peripheral.resets) == 3
    assert kept.resets == [0, events[2].now]


# Port E's registers and PE0 (MC68331UM Table D-3, D.3.6 to D.3.8).
PORTE0, DDRE, PEPAR = SIM + 0x10, SIM + 0x14, SIM + 0x16
PE0 = 0x01


def test_the_host_sees_a_pulse_on_a_port_pin():
    program = (
        store_word(PEPAR, 0xFF & ~PE0)
        + store_word(DDRE, PE0)
        + store_word(PORTE0, PE0)
        + store_word(PORTE0, 0)
    )
    chip = board(program, Recorder())[0].build()
    chip.run_for(1000)
    assert chip.port_e_outputs() == (PE0, 0)
    assert chip.port_c_outputs() == (0, 0)
    assert chip.port_f_outputs() == (0, 0)
    changes = chip.take_port_changes()
    assert [change[1:] for change in changes] == [
        (mc683xx.PortName.E, PE0, 0),
        (mc683xx.PortName.E, PE0, PE0),
        (mc683xx.PortName.E, PE0, 0),
    ]
    assert changes[0][0] < changes[1][0] < changes[2][0]
    assert chip.take_port_changes() == []


# Where WIRING ends and the program after it starts.
AFTER_WIRING = START + 2 * len(WIRING)
ADDQ_WORD_1_D0 = 0x5240
# The RAM page the hook tests use, the first: 2 Kbytes.
PAGE_BYTES = 2048
RAM_WORD = RAM_BASE + 0x10


def ram_round_trip():
    """Writes D0 to RAM_WORD, then reads it back into D1."""
    return WIRING + [
        MOVE_WORD_D0_TO_ABSOLUTE, RAM_WORD >> 16, RAM_WORD & 0xFFFF,
        MOVE_WORD_ABSOLUTE_TO_D1, RAM_WORD >> 16, RAM_WORD & 0xFFFF,
    ]


def test_a_code_hook_stops_before_its_instruction():
    builder, ram, _ = board(ram_round_trip(), Recorder())
    chip = builder.build()
    chip.write_register("A7", RAM_BASE + 0x100)
    chip.write_memory(ram, 0x100, struct.pack(">I", 0xCAFEF00D))
    seen = []

    def at(instruction):
        seen.append(instruction)
        return True

    hook = chip.add_code_hook(at, AFTER_WIRING, AFTER_WIRING)
    run = chip.run_for(2000)
    assert run.stop == mc683xx.Stop.Hook(hook)
    assert chip.read_register("PC") == AFTER_WIRING
    (instruction,) = seen
    assert instruction.pc == AFTER_WIRING
    assert instruction.registers["A7"] == RAM_BASE + 0x100
    assert instruction.stack_top == 0xCAFEF00D
    # the next run executes it, its hook not called again
    run = chip.run_for(2000)
    assert run.stop == mc683xx.Stop.Budget()
    assert len(seen) == 1
    assert chip.remove_hook(hook)
    assert not chip.remove_hook(hook)


def test_a_block_hook_sees_a_handler_start():
    builder, _, _ = board(WIRING + [MOVE_WORD_ABSOLUTE_TO_D1, PERIPHERAL_BASE >> 16, 0x0020], Faulting())
    chip = builder.build()
    starts = []
    hook = chip.add_block_hook(lambda i: starts.append(i.pc) or i.pc == BUS_ERROR_HANDLER)
    run = chip.run_for(2000)
    assert run.stop == mc683xx.Stop.Hook(hook)
    # the first block from reset, then the handler's
    assert starts == [START, BUS_ERROR_HANDLER]
    assert chip.read_register("PC") == BUS_ERROR_HANDLER


def test_a_pc_write_after_a_stop_starts_a_block():
    builder, _, _ = board(ram_round_trip(), Recorder())
    chip = builder.build()
    starts = []
    chip.add_block_hook(lambda i: starts.append(i.pc) or False)
    hook = chip.add_code_hook(lambda i: True, AFTER_WIRING, AFTER_WIRING)
    assert chip.run_for(2000).stop == mc683xx.Stop.Hook(hook)
    assert starts == [START]
    second_move = AFTER_WIRING + 6
    chip.write_register("PC", second_move)
    chip.run_for(2000)
    assert starts[:2] == [START, second_move]


def test_memory_hooks_see_reads_and_writes():
    builder, _, _ = board(ram_round_trip(), Recorder())
    chip = builder.build()
    chip.write_register("D0", 0x1234)
    writes, reads = [], []
    write_hook = chip.add_write_hook(lambda a: writes.append(a) or True, RAM_BASE, RAM_BASE + PAGE_BYTES - 1)
    chip.add_read_hook(lambda a: reads.append(a), RAM_BASE, RAM_BASE + PAGE_BYTES - 1)
    run = chip.run_for(2000)
    assert run.stop == mc683xx.Stop.Hook(write_hook)
    assert [(a.address, a.size, a.value) for a in writes] == [(RAM_WORD, 2, 0x1234)]
    assert writes[0].function_code == 5
    assert reads == []
    chip.run_for(2000)
    assert [(a.address, a.size, a.value) for a in reads] == [(RAM_WORD, 2, 0x1234)]
    assert chip.read_register("D1") == 0x1234


def test_an_invalid_hook_and_an_exception_hook_see_a_bus_error():
    builder, _, _ = board(WIRING + [MOVE_WORD_ABSOLUTE_TO_D1, PERIPHERAL_BASE >> 16, 0x0020], Faulting())
    chip = builder.build()
    invalid, entered = [], []
    chip.add_invalid_hook(invalid.append)
    hook = chip.add_exception_hook(lambda e: entered.append(e) or True)
    run = chip.run_for(2000)
    assert run.stop == mc683xx.Stop.Hook(hook)
    (access,) = invalid
    assert (access.address, access.size, access.cycle, access.value, access.reason) == (
        PERIPHERAL_BASE + 0x20, 2, mc683xx.CycleKind.READ, None, mc683xx.Invalid.BUS_ERROR,
    )
    (entry,) = entered
    assert (entry.vector, entry.handler, entry.level) == (BUS_ERROR, BUS_ERROR_HANDLER, None)
    assert entry.stack == chip.read_register("SSP")
    # stopped with the handler not yet run
    assert chip.read_register("PC") == BUS_ERROR_HANDLER


def test_an_invalid_hook_stops_once_the_exception_is_taken():
    builder, _, _ = board(WIRING + [MOVE_WORD_ABSOLUTE_TO_D1, PERIPHERAL_BASE >> 16, 0x0020], Faulting())
    chip = builder.build()
    hook = chip.add_invalid_hook(lambda a: True)
    assert chip.run_for(2000).stop == mc683xx.Stop.Hook(hook)
    assert chip.read_register("PC") == BUS_ERROR_HANDLER


def test_a_hook_exception_is_raised_by_run():
    builder, _, _ = board(ram_round_trip(), Recorder())
    chip = builder.build()

    def broken(instruction):
        return 1 // 0

    chip.add_code_hook(broken, AFTER_WIRING, AFTER_WIRING)
    with pytest.raises(ZeroDivisionError):
        chip.run_for(2000)
    assert chip.read_register("PC") == AFTER_WIRING
    # once, and the next run is not stopped by it
    assert chip.run_for(2000).stop == mc683xx.Stop.Budget()
    with pytest.raises(TypeError):
        chip.add_code_hook(42)


def test_a_hook_cannot_call_into_the_running_chip():
    builder, _, _ = board(ram_round_trip(), Recorder())
    chip = builder.build()
    chip.add_code_hook(lambda i: chip.read_register("D0"), AFTER_WIRING, AFTER_WIRING)
    # the chip is borrowed by its run
    with pytest.raises(RuntimeError):
        chip.run_for(2000)
    assert chip.read_register("PC") == AFTER_WIRING


def test_a_write_to_a_protected_page_is_a_bus_error():
    builder, _, _ = board(ram_round_trip(), Recorder())
    chip = builder.build()
    chip.protect(mc683xx.Space.SUPERVISOR, RAM_BASE, PAGE_BYTES, "rx")
    invalid = []
    chip.add_invalid_hook(lambda a: invalid.append(a) or True)
    chip.run_for(2000)
    (access,) = invalid
    assert (access.address, access.cycle, access.reason) == (
        RAM_WORD, mc683xx.CycleKind.WRITE, mc683xx.Invalid.PROTECTED,
    )
    assert chip.read_register("PC") == BUS_ERROR_HANDLER
    with pytest.raises(ValueError):
        chip.protect(mc683xx.Space.USER, RAM_BASE, 100, "r")
    with pytest.raises(ValueError):
        chip.protect(mc683xx.Space.USER, RAM_BASE, PAGE_BYTES, "rq")


def counting(*models, qspi=None):
    """A loop that counts D0 and stores it in RAM and to the peripheral, a
    `Register`, whose cycles the trace sees; `models` on no pin, `qspi`
    on the QSPI."""
    loop = [
        ADDQ_WORD_1_D0,
        MOVE_WORD_D0_TO_ABSOLUTE, RAM_WORD >> 16, RAM_WORD & 0xFFFF,
        MOVE_WORD_D0_TO_ABSOLUTE, PERIPHERAL_BASE >> 16, 0x0010,
    ]
    builder, ram, _ = board(WIRING + loop + [short_branch(BRA_SHORT, len(loop) + 1)], Register())
    for model in models:
        builder.unwired_peripheral(model)
    if qspi is not None:
        builder.qspi(qspi)
    chip = builder.build()
    chip.set_tracing(accesses=True, exceptions=True)
    return chip, ram


def traced(chip, ram, budget):
    chip.run_for(budget)
    accesses = [
        (e.now, e.address, e.write, e.value)
        for e in chip.take_events()
        if isinstance(e, mc683xx.Event.Access)
    ]
    return accesses, chip.registers(), chip.now, chip.memory(ram)


def test_a_restored_chip_runs_the_same():
    chip, ram = counting()
    chip.run_for(3000)
    chip.take_events()
    snapshot = chip.save()
    assert isinstance(snapshot, bytes)
    first = traced(chip, ram, 5000)
    assert len(first[0]) > 20
    chip.restore(snapshot)
    assert traced(chip, ram, 5000) == first
    # and into another chip built the same way
    other, other_ram = counting()
    other.run_for(1234)
    other.take_events()
    other.restore(snapshot)
    assert traced(other, other_ram, 5000) == first


def test_a_snapshot_holds_the_peripherals():
    chip, _ = counting()
    chip.run_for(3000)
    peripheral = chip.peripheral(0)
    snapshot = chip.save()
    value = peripheral.value
    assert value > 0
    chip.run_for(3000)
    assert peripheral.value != value
    chip.restore(snapshot)
    assert peripheral.value == value


def test_a_peripheral_without_save_fails_the_save_naming_it():
    chip, _ = counting(Fixed(), Recorder())
    with pytest.raises(mc683xx.StateError, match="^peripheral 2: a Python peripheral cannot save its state$"):
        chip.save()
    saving, _ = counting(Fixed(), Fixed())
    with pytest.raises(mc683xx.StateError, match="^peripheral 2: "):
        chip.restore(saving.save())


class SaveOnly:
    """A model, on the bus or the QSPI, with `save` but no `restore`."""

    def transfer(self, transfer):
        return 0

    def save(self):
        return b""


class RestoreOnly:
    """A model, on the bus or the QSPI, with `restore` but no `save`."""

    def transfer(self, transfer):
        return 0

    def restore(self, snapshot):
        pass


def test_a_peripheral_with_save_or_restore_alone_cannot_save():
    for half in (SaveOnly(), RestoreOnly()):
        chip, _ = counting(Fixed(), half)
        chip.run_for(3000)
        before = chip.registers()
        with pytest.raises(
            mc683xx.StateError, match="^peripheral 2: a Python peripheral cannot save its state$"
        ):
            chip.save()
        saving, _ = counting(Fixed(), Fixed())
        with pytest.raises(mc683xx.StateError, match="^peripheral 2: "):
            chip.restore(saving.save())
        assert chip.registers() == before
    for half in (SaveOnly(), RestoreOnly()):
        chip, _ = counting(qspi=half)
        with pytest.raises(
            mc683xx.StateError, match="^the QSPI peripheral: a Python peripheral cannot save its state$"
        ):
            chip.save()
        saving, _ = counting(qspi=Tally())
        with pytest.raises(mc683xx.StateError, match="^the QSPI peripheral: "):
            chip.restore(saving.save())


def test_a_keyboard_interrupt_in_a_restore_is_raised_and_changes_nothing():
    class Interrupted(Fixed):
        def restore(self, snapshot):
            raise KeyboardInterrupt

    chip, _ = counting(Fixed(), Interrupted())
    chip.run_for(3000)
    snapshot = chip.save()
    chip.run_for(3000)
    before = chip.save()
    with pytest.raises(KeyboardInterrupt):
        chip.restore(snapshot)
    assert chip.save() == before


def test_an_exception_a_peripheral_save_raises_is_raised():
    class Raising(Fixed):
        def save(self):
            return 1 // 0

    chip, _ = counting(Raising())
    with pytest.raises(ZeroDivisionError):
        chip.save()


def test_a_restore_into_another_count_of_peripherals_is_refused_and_changes_nothing():
    chip, _ = counting()
    chip.run_for(3000)
    snapshot = chip.save()
    other, other_ram = counting(Fixed())
    other.run_for(1234)
    before = other.save()
    with pytest.raises(mc683xx.StateError, match="peripherals"):
        other.restore(snapshot)
    assert other.save() == before


def test_a_peripheral_that_refuses_its_snapshot_leaves_the_chip_as_it_was():
    class Built:
        """A model that takes only a snapshot of its own build."""

        def __init__(self, build):
            self.build = build

        def save(self):
            return self.build

        def restore(self, snapshot):
            if snapshot != self.build:
                raise ValueError("another build's snapshot")

    model = Built(b"a")
    chip, _ = counting(model)
    chip.run_for(3000)
    snapshot = chip.save()
    chip.run_for(3000)
    model.build = b"b"
    before = chip.save()
    value = chip.peripheral(0).value
    with pytest.raises(ValueError, match="another build's snapshot"):
        chip.restore(snapshot)
    assert chip.save() == before
    assert chip.peripheral(0).value == value


class Tally:
    """A QSPI peripheral that answers each transfer with a count of
    them, which it saves."""

    def __init__(self, count=0):
        self.count = count

    def transfer(self, transfer):
        self.count += 1
        return self.count & 0xFF

    def save(self):
        return struct.pack(">H", self.count)

    def restore(self, snapshot):
        if len(snapshot) != 2:
            raise ValueError("not a Tally snapshot")
        (self.count,) = struct.unpack(">H", snapshot)


def test_a_snapshot_holds_the_qspi_peripheral():
    tally = Tally(3)
    chip, _ = counting(qspi=tally)
    chip.run_for(3000)
    snapshot = chip.save()
    tally.count = 9
    chip.run_for(3000)
    chip.restore(snapshot)
    assert tally.count == 3
    other_tally = Tally(5)
    other, _ = counting(qspi=other_tally)
    other.restore(snapshot)
    assert other_tally.count == 3
    assert other.save() == snapshot


def test_a_qspi_peripheral_without_save_fails_the_save_naming_it():
    chip, _ = counting(qspi=Peripheral(0))
    with pytest.raises(
        mc683xx.StateError, match="^the QSPI peripheral: a Python peripheral cannot save its state$"
    ):
        chip.save()
    saving, _ = counting(qspi=Tally())
    with pytest.raises(mc683xx.StateError, match="^the QSPI peripheral: "):
        chip.restore(saving.save())


def test_a_snapshot_with_another_qspi_attachment_is_refused():
    with_qspi, _ = counting(qspi=Tally())
    chip, _ = counting()
    before = chip.save()
    with pytest.raises(mc683xx.StateError, match="the QSPI peripheral"):
        chip.restore(with_qspi.save())
    assert chip.save() == before


def test_a_qspi_peripheral_that_refuses_its_snapshot_leaves_the_chip_as_it_was():
    class Refusing(Tally):
        """Takes back only its own snapshot."""

        def restore(self, snapshot):
            if snapshot != self.save():
                raise ValueError("refused")

    tally = Tally(4)
    saving, _ = counting(qspi=tally)
    saving.run_for(3000)
    snapshot = saving.save()
    refusing = Refusing(7)
    chip, _ = counting(qspi=refusing)
    chip.run_for(1234)
    before = chip.save()
    value = chip.peripheral(0).value
    with pytest.raises(ValueError, match="refused"):
        chip.restore(snapshot)
    assert chip.save() == before
    assert chip.peripheral(0).value == value
    assert refusing.count == 7


def test_a_snapshot_of_another_format_is_refused():
    chip, _ = counting()
    snapshot = bytearray(chip.save())
    with pytest.raises(mc683xx.StateError):
        chip.restore(b"")
    snapshot[0] ^= 1
    with pytest.raises(ValueError):
        chip.restore(bytes(snapshot))


def mapped(peripheral):
    """The board with RAM and the peripheral mapped by the host's own
    cycles, not the firmware's."""
    builder, ram, number = board([0x60FE], peripheral)
    chip = builder.build()
    for address, value in ((CSBAR0, csbar(RAM_BASE)), (CSOR0, CSOR_FAST), (CSBAR1, csbar(PERIPHERAL_BASE)), (CSOR1, CSOR_FAST)):
        chip.bus_write(address, 2, value)
    return chip, ram, number


def test_the_host_reaches_memory_and_peripherals_through_the_decode():
    peripheral = Recorder(0xBEEF)
    chip, ram, _ = mapped(peripheral)
    now = chip.now
    chip.bus_write(RAM_BASE, 4, 0x12345678)
    assert chip.read_memory(ram, 0, 4) == b"\x12\x34\x56\x78"
    assert chip.bus_read(RAM_BASE, 4) == 0x12345678
    assert chip.peek(RAM_BASE + 2, 2) == 0x5678
    assert chip.peek(CSBAR1, 2) == csbar(PERIPHERAL_BASE)
    assert chip.bus_read(PERIPHERAL_BASE + 4, 2) == 0xBEEF
    chip.bus_write(PERIPHERAL_BASE + 1, 1, 0x42)
    with pytest.raises(mc683xx.UnpeekableError):
        chip.peek(PERIPHERAL_BASE, 2)
    assert peripheral.reads == [("CS1", PERIPHERAL_BASE + 4, 2)]
    assert peripheral.writes == [("CS1", PERIPHERAL_BASE + 1, 1, 0x42)]
    assert chip.now == now


def test_host_cycle_mistakes():
    chip, _, _ = mapped(Faulting())
    with pytest.raises(mc683xx.BusError):
        chip.bus_read(PERIPHERAL_BASE, 2)
    for mistake in (
        lambda: chip.bus_read(RAM_BASE + 1, 2),
        lambda: chip.bus_read(RAM_BASE, 3),
        lambda: chip.peek(RAM_BASE, 2, 8),
        lambda: chip.bus_write(RAM_BASE, 1, 0x100),
    ):
        with pytest.raises(ValueError):
            mistake()


def test_a_peripheral_exception_is_raised_by_the_host_cycle():
    chip, _, _ = mapped(Broken())
    with pytest.raises(ZeroDivisionError):
        chip.bus_read(PERIPHERAL_BASE, 2)
    assert chip.run_for(100).stop == mc683xx.Stop.Budget()


def test_runs_until_rising_targets_are_one_long_run():
    long, _, _ = mapped(Recorder())
    long.run_until(20_000)
    short, _, _ = mapped(Recorder())
    for target in range(0, 20_001, 7):
        short.run_until(target)
    short.run_until(20_000)
    assert (short.now, short.registers(), short.instructions) == (long.now, long.registers(), long.instructions)
    assert long.now >= 20_000
    assert long.run_until(0).cycles == 0


def test_enums_and_values_hash_as_they_compare() -> None:
    def values() -> list[object]:
        return [
            mc683xx.Stop.Budget(),
            mc683xx.Stop.Halted(mc683xx.Halt.DOUBLE_BUS_FAULT),
            mc683xx.Halt.DOUBLE_BUS_FAULT,
            mc683xx.ResetCause.WATCHDOG,
            mc683xx.Space.USER,
            mc683xx.CycleKind.READ,
            mc683xx.Invalid.PROTECTED,
            mc683xx.FaultCause.BUS_ERROR,
            mc683xx.ClockPolarity.IDLE_LOW,
            mc683xx.ClockPhase.CHANGE_LEADING,
            mc683xx.Ending.DETACHED,
            mc683xx.Frame.Data(0x41),
            mc683xx.Output.Level(True),
            mc683xx.Serial(mc683xx.Frame.Data(0x41), 9600, 10),
            mc683xx.Channel.A,
            mc683xx.PortName.F,
            mc683xx.OutputChange.PortGp(1, 0),
            mc683xx.Role.IACK,
            mc683xx.Lane.UPPER,
            mc683xx.Deviations(misaligned=True),
        ]

    for one, other in zip(values(), values()):
        assert one == other and hash(one) == hash(other)
    assert len(set(values())) == len(values())
    assert {mc683xx.Ending.DETACHED: 1}[mc683xx.Ending.DETACHED] == 1


def test_a_halted_chip_spends_a_long_run_as_time() -> None:
    # A boot peripheral that ends every cycle in a bus error faults the
    # reset vectors (CPU32RM 6.2.1): the chip halts at once, and a run
    # longer than one quantum spends all of it.
    builder = mc683xx.Builder()
    builder.peripheral(Faulting(), ["CSBOOT"])
    chip = builder.build()
    assert chip.halted is not None
    run = chip.run_for(1_000_000)
    assert run.stop == mc683xx.Stop.Halted(mc683xx.Halt.DOUBLE_BUS_FAULT)
    assert run.cycles == 1_000_000


# CSBAR2, CSOR2 and PEPAR (MC68331UM Table D-3, D.3.8); CS2's option
# externally terminated, as CSOR's DSACK field %1111 (SIMRM 7.10.3).
CSBAR2, CSOR2, PEPAR_REGISTER = SIM + 0x54, SIM + 0x56, SIM + 0x16
CSOR_EXTERNAL = 0x7BF0
DSACK_BASE = 0x600000


def test_a_peripheral_s_unassigned_dsack_is_an_event():
    builder = booting([])
    number = builder.peripheral(Recorder(0x1234), ["CS2"])
    builder.dsack(number, mc683xx.Dsack.BOTH)
    with pytest.raises(IndexError):
        builder.dsack(9, mc683xx.Dsack.DSACK0)
    chip = builder.build()
    chip.bus_write(CSBAR2, 2, DSACK_BASE >> 8)
    chip.bus_write(CSOR2, 2, CSOR_EXTERNAL)
    assert chip.bus_read(DSACK_BASE, 2) == 0x1234
    # PEPAR 0: PE0 and PE1 port E pins, so no DSACK ends the cycle
    # (SIMRM 9.1), and with no bus monitor nothing does
    chip.bus_write(PEPAR_REGISTER, 2, 0)
    chip.take_events()
    with pytest.raises(mc683xx.BusError):
        chip.bus_read(DSACK_BASE, 2)
    [event] = chip.take_events()
    assert isinstance(event, mc683xx.Event.Dsack)
    assert (event.now, event.fault) == (0, mc683xx.DsackFault.Unassigned("CS2"))


class Reset:
    """A QSPI peripheral that records its resets."""

    def __init__(self):
        self.resets = []

    def transfer(self, transfer):
        return 0

    def reset(self, now):
        self.resets.append(now)


def test_the_qspi_reset_scope_limits_the_qspi_peripheral_s_resets():
    def resets(scope):
        builder = booting([])
        peripheral = Reset()
        builder.qspi(peripheral)
        if scope is not None:
            builder.qspi_reset_scope(scope)
        chip = builder.build()
        chip.reset()
        return len(peripheral.resets)

    assert resets(None) == 2, "power-on and RESET"
    assert resets([mc683xx.ResetCause.EXTERNAL]) == 1
    assert resets([]) == 0


class Stopping(Alarm):
    """At `at`, asks the run through `request` (a stop or a sync)."""

    def __init__(self, at, request):
        super().__init__(at)
        self.request = request

    def advance(self, now):
        super().advance(now)
        if now >= self.at:
            self.request.request()


def test_a_peripheral_s_stop_and_sync_requests_end_the_run():
    builder = booting([])
    builder.unwired_peripheral(Stopping(1000, builder.stop_request()))
    chip = builder.build()
    run = chip.run_for(10_000)
    assert run.stop == mc683xx.Stop.Requested()
    assert 1000 <= chip.now < 10_000

    builder = booting([])
    builder.unwired_peripheral(Stopping(1000, builder.sync_request()))
    chip = builder.build()
    run = chip.run_for(10_000)
    assert run.stop == mc683xx.Stop.Budget()
    assert 1000 <= run.cycles < 10_000


class Switch:
    """Diverts `memory` to `peripheral` while written nonzero: a flash's
    command logic."""

    def __init__(self, diversion):
        self.diversion = diversion
        self.memory = self.peripheral = 0

    def read(self, cycle):
        return 0

    def write(self, cycle, value):
        if value:
            self.diversion.divert(self.memory, self.peripheral)
        else:
            self.diversion.release(self.memory)


# CS1 at $200000 and CS2 at $300000, fast (MC68331UM Table D-3).
DIVERTED_BASE, SWITCH_BASE = 0x200000, 0x300000


def test_a_peripheral_diverts_a_memory_to_another_and_back():
    program = (
        store_word(CSBAR1, csbar(DIVERTED_BASE))
        + store_word(CSOR1, CSOR_FAST)
        + store_word(CSBAR2, csbar(SWITCH_BASE))
        + store_word(CSOR2, CSOR_FAST)
        + load_word(DIVERTED_BASE, 0)
        + store_word(SWITCH_BASE, 1)
        + load_word(DIVERTED_BASE, 1)
        + store_word(SWITCH_BASE, 0)
        + load_word(DIVERTED_BASE, 2)
    )
    builder = booting(program)
    memory = builder.memory(bytes([0x11]) * 2048, ["CS1"], writable=False)
    switch = Switch(builder.diversion())
    builder.peripheral(switch, ["CS2"])
    switch.memory, switch.peripheral = memory, builder.unwired_peripheral(Recorder(0xBEEF))
    chip = builder.build()
    chip.run_for(2000)
    assert [chip.read_register(f"D{n}") & 0xFFFF for n in range(3)] == [0x1111, 0xBEEF, 0x1111]
    with pytest.raises(IndexError):
        switch.diversion.release(9)


def test_a_host_with_no_board_takes_port_qs_changes() -> None:
    # PQSPAR clear (port QS is I/O out of reset), DDRQS bit 1 an
    # output, then PORTQS bit 1 low and high (QSMRM 3.5)
    chip, _, _ = mapped(Recorder())
    chip.bus_write(QSM + 0x17, 1, 0x02)
    chip.bus_write(QSM + 0x15, 1, 0x00)
    chip.bus_write(QSM + 0x15, 1, 0x02)
    changes = chip.take_qsm_changes()
    assert [levels & 0x02 for (_, driven, levels) in changes if driven & 0x02][-2:] == [0, 2]
    assert chip.take_qsm_changes() == []


class Narrow(Recorder):
    """A Recorder whose DSACK is 8 bits at even words, 16 at odd ones."""

    def read(self, cycle):
        super().read(cycle)
        return self.value & (0xFF if cycle.size == 1 else 0xFFFF)

    def dsack_port(self, cycle):
        return 8 if cycle.address & 2 == 0 else 16


def test_a_bus_peripheral_decodes_its_own_window():
    window = 0x800000
    builder = mc683xx.Builder()
    builder.memory(rom([0x60FE]), ["CSBOOT"], writable=False)
    peripheral = Narrow(0x1234)
    number = builder.bus_peripheral(
        peripheral, windows=[(window, 0x100)], acknowledges=[4], dsack=mc683xx.Dsack.DSACK1
    )
    with pytest.raises(ValueError):
        builder.bus_peripheral(Recorder(), windows=[(0xFFFF00, 0x200)], dsack=mc683xx.Dsack.DSACK1)
    with pytest.raises(ValueError):
        builder.bus_peripheral(Recorder(), acknowledges=[0], dsack=mc683xx.Dsack.DSACK1)
    chip = builder.build()
    assert chip.peripheral(number) is peripheral
    chip.bus_read(window, 2)
    chip.bus_read(window + 2, 2)
    assert peripheral.reads == [
        (None, window, 1),
        (None, window + 1, 1),
        (None, window + 2, 2),
    ], "no select; DSACK0 splits the even word"
