/*
 * The demo's vector table and reset code: the SIM set up, .data copied
 * from ROM, .bss zeroed, then main.  fault_entry takes every exception
 * the demo does not expect.
 */
#include "board.h"

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
	.long	pit_isr
	FAULTS_UP_TO(SCI_VECTOR)
	.long	sci_isr
	FAULTS_UP_TO(GPT_IC1_VECTOR)
	.long	gpt_ic1_isr
	FAULTS_UP_TO(GPT_TOF_VECTOR)
	.long	gpt_overflow_isr
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

/* Any exception the demo does not expect, and the address error `f`
 * makes.  fault_report prints the frame; then the faulted context is
 * abandoned, as a longjmp would: the stack back to its top, interrupts
 * unmasked, and on to the command loop.  No RTE: a format $C frame
 * would rerun the faulting instruction (CPU32RM 6.3.2.3). */
	.globl	fault_entry
fault_entry:
	move.l	%sp, -(%sp)
	jsr	fault_report
	lea	__stack_top, %sp
	move.w	#SR_SUPERVISOR, %sr
	jmp	command_loop
