#!/bin/sh
# A source-level gdb session against the runner on the demo board
# (docs/gdb-firmware.md): `make gdb`.  gdb loads demo.elf, breaks in an
# ISR, unwinds through its exception frame, watches `ticks`, steps C,
# catches the `f` command's address error at fault_entry, then
# detaches and the run finishes; its output and the console's are
# checked.  Needs gdb with the m68k target: gdb-multiarch, or set GDB.
#
# usage: gdb-demo.sh
set -eu

gdb=${GDB:-gdb-multiarch}
if ! command -v "$gdb" >/dev/null 2>&1; then
	echo "$gdb not found: install gdb-multiarch, or set GDB to an m68k gdb" >&2
	exit 2
fi
limit=${GDB_TIMEOUT:-60}

work=$(mktemp -d)
runner=
cleanup() {
	if [ -n "$runner" ]; then
		kill "$runner" 2>/dev/null || true
	fi
	rm -rf "$work"
}
trap cleanup EXIT

# The console reads a file, not the terminal gdb has.
printf 'p\nf\nt\n' >"$work/console.in"

cargo build -q -p mc683xx-run
timeout "$limit" target/debug/mc683xx-run examples/boards/demo.toml \
	--bind console=stdio --seconds 3 --input-after 0.01 --gdb 127.0.0.1:0 \
	<"$work/console.in" >"$work/console.out" 2>"$work/runner.err" &
runner=$!
tries=0
until grep -q 'waiting for gdb on' "$work/runner.err"; do
	tries=$((tries + 1))
	if [ "$tries" -gt 100 ]; then
		cat "$work/runner.err" >&2
		exit 1
	fi
	sleep 0.1
done
address=$(sed -n 's/^mc683xx-run: waiting for gdb on //p' "$work/runner.err")

# At an exception handler's first instruction the frame is at SP: SR,
# then the PC (CPU32RM 6.4).  bt cannot unwind it: GCC describes an
# interrupt_handler's frame as a call's.
timeout "$limit" "$gdb" -batch -nx examples/boards/demo.elf \
	-ex 'directory examples/firmware/demo' \
	-ex "target remote $address" \
	-ex 'break pit_isr' \
	-ex 'continue' \
	-ex 'x/a $sp+2' \
	-ex 'print ticks' \
	-ex 'info registers system' \
	-ex 'delete' \
	-ex 'watch ticks' \
	-ex 'continue' \
	-ex 'continue' \
	-ex 'delete' \
	-ex 'break primes' \
	-ex 'continue' \
	-ex 'bt' \
	-ex 'list' \
	-ex 'next' \
	-ex 'next' \
	-ex 'finish' \
	-ex 'delete' \
	-ex 'break fault_entry' \
	-ex 'continue' \
	-ex 'x/6hx $sp' \
	-ex 'x/a $sp+2' \
	-ex 'delete' \
	-ex 'detach' \
	>"$work/gdb.out" 2>&1 || status=$?
cat "$work/gdb.out"
if [ "${status:-0}" -ne 0 ]; then
	echo "gdb-demo: $gdb failed or timed out" >&2
	exit 1
fi
wait "$runner" || status=$?
runner=
cat "$work/console.out"
echo
if [ "${status:-0}" -ne 0 ]; then
	cat "$work/runner.err" >&2
	echo "gdb-demo: the runner failed or timed out" >&2
	exit 1
fi

# The address error's frame is format $C, vector offset $00C (CPU32RM
# 6.4, Figure 6-6), its fault address $100001.
for expected in \
	'Breakpoint 1, pit_isr () at main.c' \
	'^0x10[0-9a-f]*:	0x[0-9a-f]* <[a-z_]*+[0-9]*>$' \
	'\$1 = 0' \
	'vbr  *0x0' \
	'Old value = 0' \
	'New value = 1' \
	'primes () at main.c' \
	'#1 .* in command_loop () at main.c' \
	'for (int n = 0; n < PRIME_LIMIT; n++)' \
	'^command_loop () at main.c' \
	'fault_entry () at ./crt0.S' \
	'0xc00c	0x0010	0x0001' \
	'<fault+' \
	'detached'; do
	if ! grep -q "$expected" "$work/gdb.out"; then
		echo "gdb-demo: no '$expected' in gdb's output" >&2
		exit 1
	fi
done
for expected in \
	'168 primes below 1000' \
	'exception: vector 3, format \$c' \
	'uptime '; do
	if ! grep -q "$expected" "$work/console.out"; then
		echo "gdb-demo: no '$expected' on the console" >&2
		exit 1
	fi
done
echo "gdb-demo: ok"
