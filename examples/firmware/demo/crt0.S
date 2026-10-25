/*
 * The demo's vector table and reset code: the SIM set up, .data copied
 * from ROM, .bss zeroed, then main.  Each interrupt enters through a
 * stub here that calls a plain C function and returns with RTE;
 * fault_entry takes every exception the demo does not expect.
 *
 * The stubs and fault_entry carry call frame information written by
 * hand, so a debugger unwinds from them into the interrupted code: a
 * C function's describes a call's frame, not an exception's.
 */
#include "board.h"

/* DWARF call frame instructions and expression operations (DWARF 4,
 * 6.4.2 and 2.5.1), for what the .cfi directives cannot say. */
#define DW_CFA_def_cfa_expression 0x0f
#define DW_CFA_expression 0x10
#define DW_OP_dup 0x12
#define DW_OP_swap 0x16
#define DW_OP_minus 0x1c
#define DW_OP_mul 0x1e
#define DW_OP_plus 0x22
#define DW_OP_shr 0x25
#define DW_OP_eq 0x29
#define DW_OP_lit0 0x30
#define DW_OP_breg15 0x7f /* SP plus an operand */
#define DW_OP_deref_size 0x94
/* GCC's DWARF numbers for the m68k: SP is A7, 15; the PC is the return
 * address column, 24. */
#define DWARF_PC 24

/* The scratch registers a C function may change, saved by the
 * interrupt stubs, and their bytes. */
#define SCRATCH_REGISTERS %d0-%d1/%a0-%a1
#define SCRATCH_BYTES 16

	.cfi_sections .debug_frame

/* fault_entry in each vector from the next up to `vector`, exclusive. */
#define FAULTS_UP_TO(vector) \
	.rept (vector) - (. - vectors) / 4; .long fault_entry; .endr

/* The vector table at ROM_BASE, where VBR points out of reset
 * (CPU32RM 6.1.1, Table 6-1). */
	.section .vectors, "a"
	.globl	vectors
vectors:
	.long	__stack_top		/* 0: reset SSP */
	.long	reset			/* 1: reset PC */
	/* 2 to $3F: bus error, address error, illegal instruction and the
	 * rest the CPU defines, spurious interrupt (24) among them. */
	FAULTS_UP_TO(PIT_VECTOR)
	.long	pit_entry
	FAULTS_UP_TO(SCI_VECTOR)
	.long	sci_entry
	FAULTS_UP_TO(GPT_IC1_VECTOR)
	.long	gpt_ic1_entry
	FAULTS_UP_TO(GPT_TOF_VECTOR)
	.long	gpt_overflow_entry
	FAULTS_UP_TO(VECTOR_COUNT)
	.if	. - vectors - VECTOR_COUNT * 4
	.error	"the vector table is not 256 vectors"
	.endif

	.text
	.globl	reset
reset:
	/* The software watchdog off, the halt and bus monitors on: a
	 * cycle nothing answers ends in a bus error.  SYPCR takes one
	 * write after reset (MC68331UM D.3.12). */
	move.b	#SYPCR_HME + SYPCR_BME, SYPCR_ADDRESS
	move.w	#SYNCR_VALUE, SYNCR_ADDRESS
	/* SLOCK is in SYNCR's low byte (MC68331UM D.3.3). */
1:	btst	#SYNCR_SLOCK_BIT, SYNCR_ADDRESS + 1
	beq.s	1b
	/* The boot ROM: 64 Kbytes at ROM_BASE, read only, no wait states. */
	move.w	#(ROM_BASE >> CSBAR_ADDRESS_SHIFT) + CSBAR_BLKSZ_64K, CSBARBT_ADDRESS
	move.w	#CSOR_BYTE_BOTH + CSOR_RW_READ + CSOR_DSACK_FAST + CSOR_SPACE_SU, CSORBT_ADDRESS
	/* RAM on CS0: 64 Kbytes at RAM_BASE, a 16-bit port. */
	move.w	#(RAM_BASE >> CSBAR_ADDRESS_SHIFT) + CSBAR_BLKSZ_64K, CSBAR0_ADDRESS
	move.w	#CSOR_BYTE_BOTH + CSOR_RW_BOTH + CSOR_DSACK_FAST + CSOR_SPACE_SU, CSOR0_ADDRESS
	or.w	#CSPAR0_CS0_16BIT, CSPAR0_ADDRESS
	/* .data from its load address in ROM; the linker script aligns
	 * both to longs. */
	lea	__data_load, %a0
	lea	__data_start, %a1
	lea	__data_end, %a2
2:	cmp.l	%a2, %a1
	bcc.s	3f
	move.l	(%a0)+, (%a1)+
	bra.s	2b
3:	lea	__bss_start, %a1
	lea	__bss_end, %a2
4:	cmp.l	%a2, %a1
	bcc.s	5f
	clr.l	(%a1)+
	bra.s	4b
5:	jsr	main
	bra.s	5b

/*
 * An interrupt's entry, `name`: the scratch registers saved, `handler`
 * called, RTE.  An interrupt stacks a format $0 frame, four words
 * (CPU32RM 6.4.1, Figure 6-3): SR at SP, the PC at SP + 2, the format
 * and vector offset at SP + 6.  The interrupted code's SP, the CFA, is
 * SP + 8 here, its PC at CFA - 6.  SR has no DWARF number, so a
 * debugger shows the handler's for the interrupted frame.
 */
.macro	INTERRUPT_ENTRY name, handler
	.globl	\name
\name:
	.cfi_startproc simple
	.cfi_signal_frame
	.cfi_def_cfa %sp, FRAME_FOUR_WORD_BYTES
	/* The PC at CFA - (FRAME_FOUR_WORD_BYTES - FRAME_PC_OFFSET), the
	 * offset not a multiple of the data alignment, 4: an expression,
	 * which starts with the CFA pushed. */
	.cfi_escape DW_CFA_expression, DWARF_PC, 2, \
		DW_OP_lit0 + FRAME_FOUR_WORD_BYTES - FRAME_PC_OFFSET, DW_OP_minus
	movem.l	SCRATCH_REGISTERS, -(%sp)
	.cfi_adjust_cfa_offset SCRATCH_BYTES
	.cfi_rel_offset %d0, 0
	.cfi_rel_offset %d1, 4
	.cfi_rel_offset %a0, 8
	.cfi_rel_offset %a1, 12
	jsr	\handler
	movem.l	(%sp)+, SCRATCH_REGISTERS
	.cfi_adjust_cfa_offset -SCRATCH_BYTES
	.cfi_same_value %d0
	.cfi_same_value %d1
	.cfi_same_value %a0
	.cfi_same_value %a1
	rte
	.cfi_endproc
.endm

	INTERRUPT_ENTRY pit_entry, pit_isr
	INTERRUPT_ENTRY sci_entry, sci_isr
	INTERRUPT_ENTRY gpt_ic1_entry, gpt_ic1_isr
	INTERRUPT_ENTRY gpt_overflow_entry, gpt_overflow_isr

/*
 * fault_entry's frame, `pushed` bytes below SP: the CFA, the faulted
 * code's SP, is past a frame whose size its format gives, $0, $2 or $C
 * on the CPU32 (CPU32RM 6.4): SP + pushed + 8, plus 4 for $2, plus 16
 * for $C.  The PC is at the frame's + 2 in all three.
 */
.macro	FAULT_FRAME pushed
	.cfi_escape DW_CFA_def_cfa_expression, 20, \
		DW_OP_breg15, \pushed + FRAME_FORMAT_OFFSET, DW_OP_deref_size, 2, \
		DW_OP_lit0 + FRAME_FORMAT_SHIFT, DW_OP_shr, \
		DW_OP_dup, DW_OP_lit0 + FORMAT_SIX_WORD, DW_OP_eq, \
		DW_OP_lit0 + FRAME_SIX_WORD_BYTES - FRAME_FOUR_WORD_BYTES, DW_OP_mul, \
		DW_OP_swap, DW_OP_lit0 + FORMAT_BERR, DW_OP_eq, \
		DW_OP_lit0 + FRAME_BERR_BYTES - FRAME_FOUR_WORD_BYTES, DW_OP_mul, \
		DW_OP_plus, DW_OP_breg15, \pushed + FRAME_FOUR_WORD_BYTES, DW_OP_plus
	.cfi_escape DW_CFA_expression, DWARF_PC, 2, \
		DW_OP_breg15, \pushed + FRAME_PC_OFFSET
.endm

/* Any exception the demo does not expect, and the address error `f`
 * makes.  fault_report prints the frame; then the faulted context is
 * abandoned, as a longjmp would: the stack back to its top, interrupts
 * unmasked, and on to the command loop.  No RTE: a format $C frame
 * would rerun the faulting instruction (CPU32RM 6.3.2.3). */
	.globl	fault_entry
fault_entry:
	.cfi_startproc simple
	.cfi_signal_frame
	FAULT_FRAME 0
	move.l	%sp, -(%sp)
	FAULT_FRAME 4
	jsr	fault_report
	lea	__stack_top, %sp
	/* Nothing to unwind into from here. */
	.cfi_undefined DWARF_PC
	move.w	#SR_SUPERVISOR, %sr
	jmp	command_loop
	.cfi_endproc
