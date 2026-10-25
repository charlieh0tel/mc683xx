#!/bin/sh
# A real gdb against the runner's `--gdb` (docs/gdb.md): `make gdb`.
# The example board with a tiny counting loop as its firmware; gdb
# steps it, breaks in it, resets it and detaches, and its output is
# checked.  Needs gdb with the m68k target: gdb-multiarch, or set GDB.
#
# usage: gdb-smoke.sh
set -eu

gdb=${GDB:-gdb-multiarch}
if ! command -v "$gdb" >/dev/null 2>&1; then
	echo "$gdb not found: install gdb-multiarch, or set GDB to an m68k gdb" >&2
	exit 2
fi

work=$(mktemp -d)
runner=
cleanup() {
	if [ -n "$runner" ]; then
		kill "$runner" 2>/dev/null || true
	fi
	rm -rf "$work"
}
trap cleanup EXIT

# The reset vectors (SSP $10000, PC $400), then at $400 (CPU32RM 4):
# moveq #0,d0; addq.l #1,d0; bra.s back to the addq.
cp examples/boards/echo.toml "$work/"
python3 - "$work/echo.bin" <<'PROGRAM'
import struct
import sys

image = bytearray(b"\xff" * 0x10000)
struct.pack_into(">II", image, 0, 0x10000, 0x400)
struct.pack_into(">3H", image, 0x400, 0x7000, 0x5280, 0x60FC)
open(sys.argv[1], "wb").write(image)
PROGRAM

cargo build -q -p mc683xx-run
target/debug/mc683xx-run "$work/echo.toml" --gdb 127.0.0.1:0 --seconds 0.001 \
	2>"$work/runner.err" &
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

"$gdb" -batch -nx \
	-ex "target remote $address" \
	-ex 'info registers pc' \
	-ex 'stepi' \
	-ex 'info registers pc' \
	-ex 'break *0x404' \
	-ex 'continue' \
	-ex 'continue' \
	-ex 'info registers d0' \
	-ex 'delete' \
	-ex 'monitor reset' \
	-ex 'flushregs' \
	-ex 'info registers pc' \
	-ex 'detach' \
	>"$work/gdb.out" 2>&1 || status=$?
cat "$work/gdb.out"
if [ "${status:-0}" -ne 0 ]; then
	echo "gdb-smoke: $gdb failed" >&2
	exit 1
fi
wait "$runner"
runner=

for expected in 'pc  *0x400' 'pc  *0x402' 'd0  *0x2' 'reset'; do
	if ! grep -q "$expected" "$work/gdb.out"; then
		echo "gdb-smoke: no '$expected' in gdb's output" >&2
		exit 1
	fi
done
echo "gdb-smoke: ok"
