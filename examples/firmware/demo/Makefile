# The demo firmware (examples/boards/demo.toml): demo.elf and demo.bin,
# written to OUT.  `make demo-image` at the top runs this.
#
# No libgcc: Debian's is built for the 68020 and may hold instructions
# the CPU32 lacks.  The code needs none: CPU32 multiplies and divides
# 32 bits in hardware.

CC := m68k-linux-gnu-gcc
OBJCOPY := m68k-linux-gnu-objcopy
OUT ?= ../../boards

# -fno-tree-loop-distribute-patterns: no memset or memcpy calls for
# loops, there being no library.  The prefix map keeps the build
# directory out of the debug information.
CFLAGS := -mcpu=cpu32 -ffreestanding -nostdlib -fno-pic -static -O2 -g \
	-Wall -Wextra -fno-tree-loop-distribute-patterns \
	-fno-asynchronous-unwind-tables -fdebug-prefix-map=$(CURDIR)=. \
	-Wl,--build-id=none

SOURCES := crt0.S main.c
HEADERS := board.h regs.h

.PHONY: all
all: $(OUT)/demo.bin

$(OUT)/demo.elf: $(SOURCES) $(HEADERS) demo.ld
	$(CC) --version | head -n 1
	$(CC) $(CFLAGS) -T demo.ld -o $@ $(SOURCES)

$(OUT)/demo.bin: $(OUT)/demo.elf
	$(OBJCOPY) -O binary $< $@
