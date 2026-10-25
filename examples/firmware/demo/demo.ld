/* The demo board's memory map (board.h, examples/boards/demo.toml). */
OUTPUT_FORMAT("elf32-m68k")
OUTPUT_ARCH(m68k)
ENTRY(reset)

MEMORY
{
	rom (rx) : ORIGIN = 0x000000, LENGTH = 64K
	ram (rwx) : ORIGIN = 0x100000, LENGTH = 64K
}

SECTIONS
{
	.vectors : { KEEP(*(.vectors)) } > rom
	.text : { *(.text .text.*) } > rom
	.rodata : { *(.rodata .rodata.*) . = ALIGN(4); } > rom
	.data : ALIGN(4) {
		__data_start = .;
		*(.data .data.*)
		. = ALIGN(4);
		__data_end = .;
	} > ram AT > rom
	__data_load = LOADADDR(.data);
	.bss (NOLOAD) : ALIGN(4) {
		__bss_start = .;
		*(.bss .bss.* COMMON)
		. = ALIGN(4);
		__bss_end = .;
	} > ram
	__stack_top = ORIGIN(ram) + LENGTH(ram);

	/DISCARD/ : { *(.comment) *(.note .note.*) *(.eh_frame) }
}
