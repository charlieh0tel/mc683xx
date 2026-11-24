"""The cost of Python hooks on a bare `Machine` (docs/performance.md,
Python hooks): run it with the module built in release
(`maturin develop --release`), pinned to one core, as

    taskset -c 2 python crates/python/benches/hooks.py

Each case runs a two-instruction loop, `move.w d0,(a0)` then `bra.s`
back, for a fixed count of instructions, and prints the best of its
rounds: the instructions a second and, where a hook is called, the
microseconds a call costs over the run with no hook.  A case the
module does not have yet is skipped, so one script measures before
and after.
"""

import argparse
import struct
import time

import mc683xx

# The loop: RAM at zero, the code at CODE, A0 the address it writes.
CODE = 0x1000
TARGET = 0x2000
STACK = 0x8000
RAM_BYTES = 0x10000
# move.w d0,(a0); bra.s CODE.
LOOP = (0x3080, 0x60FC)
# An address no instruction of the loop is at.
ELSEWHERE = 0x100000


def machine() -> mc683xx.Machine:
    """The loop's machine, PC at its start."""
    cpu = mc683xx.Machine(address_bits=24)
    cpu.map(0, RAM_BYTES)
    cpu.poke_bytes(CODE, struct.pack(f">{len(LOOP)}H", *LOOP))
    cpu.write_register("SR", 0x2700)
    cpu.write_register("A7", STACK)
    cpu.write_register("A0", TARGET)
    cpu.write_register("PC", CODE)
    return cpu


def no_op(_seen: object) -> None:
    return None


def registers_a7(access: mc683xx.MemoryAccess) -> None:
    access.registers["A7"]


def register_a7(access: mc683xx.MemoryAccess) -> None:
    access.register("A7")


def install(cpu: mc683xx.Machine, case: str) -> bool:
    """Installs `case`'s hooks: whether the module has what it needs."""
    if case == "none":
        pass
    elif case == "code":
        cpu.add_code_hook(no_op)
    elif case == "code_elsewhere":
        cpu.add_code_hook(no_op, ELSEWHERE, ELSEWHERE + 1)
    elif case == "write":
        cpu.add_write_hook(no_op)
    elif case == "write_registers":
        cpu.add_write_hook(registers_a7)
    elif case == "write_register":
        if not hasattr(mc683xx.MemoryAccess, "register"):
            return False
        cpu.add_write_hook(register_a7)
    elif case == "install_remove":
        pass
    else:
        raise ValueError(case)
    return True


# Hook calls per instruction in each case.
CALLS = {
    "code": 1.0,
    "write": 0.5,
    "write_registers": 0.5,
    "write_register": 0.5,
}

CASES = (
    "none",
    "code_elsewhere",
    "code",
    "write",
    "write_registers",
    "write_register",
)


def best(case: str, instructions: int, rounds: int) -> float | None:
    """The fewest seconds of `rounds` runs of `instructions`."""
    cpu = machine()
    if not install(cpu, case):
        return None
    times = []
    for _ in range(rounds):
        start = time.perf_counter()
        cpu.run_limited(instructions=instructions)
        times.append(time.perf_counter() - start)
    return min(times)


def install_remove(rounds: int, count: int) -> float:
    """The fewest seconds of `rounds` of `count` add and remove pairs of
    a ranged code hook, on the loop's machine with 512 Kbytes more
    mapped."""
    cpu = machine()
    cpu.map(0x100000, 0x80000)
    times = []
    for _ in range(rounds):
        start = time.perf_counter()
        for _ in range(count):
            cpu.remove_hook(cpu.add_code_hook(no_op, CODE, CODE + 1))
        times.append(time.perf_counter() - start)
    return min(times)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--instructions", type=int, default=2_000_000)
    parser.add_argument("--hooked", type=int, default=200_000)
    parser.add_argument("--rounds", type=int, default=7)
    arguments = parser.parse_args()
    baseline = None
    for case in CASES:
        count = arguments.instructions if case not in CALLS else arguments.hooked
        seconds = best(case, count, arguments.rounds)
        if seconds is None:
            print(f"{case:16} skipped")
            continue
        per = seconds / count
        if case == "none":
            baseline = per
        line = f"{case:16} {1 / per / 1e6:8.2f} M instructions/s"
        if case in CALLS and baseline is not None:
            line += f"  {(per - baseline) / CALLS[case] * 1e6:6.3f} us a call"
        print(line)
    pairs = 1000
    seconds = install_remove(arguments.rounds, pairs)
    print(f"{'install_remove':16} {seconds / pairs * 1e6:8.2f} us an add and remove")


if __name__ == "__main__":
    main()
