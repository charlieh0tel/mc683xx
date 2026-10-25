/*
 * The demo board (examples/boards/demo.toml): its memory map, clocks,
 * interrupt levels and vectors, for C and crt0.S.  Expressions use +,
 * not |, the m68k assembler's comment character.
 */
#ifndef DEMO_BOARD_H
#define DEMO_BOARD_H

#include "regs.h"

/* ROM on CSBOOT, read only; RAM on CS0.  Both 16-bit, 64 Kbytes. */
#define ROM_BASE 0x000000
#define RAM_BASE 0x100000

/* The clock synthesizer's reference, EXTAL, in Hz. */
#define REFERENCE_HZ 32768
/* SYNCR Y = 63, X = 1, W = 0: 32768 * 4 * 64 * 2 = 16.777216 MHz
 * (MC68331UM 4.3.2), the part's rated maximum (MC68331UM A, Table
 * A-4). */
#define SYNCR_Y 63
#define SYNCR_VALUE (SYNCR_X + (SYNCR_Y << SYNCR_Y_SHIFT))
#define SYSTEM_HZ (REFERENCE_HZ * 4 * (SYNCR_Y + 1) * 2)

/* PIT period = PITM * 4 / EXTAL, PTP clear (MC68331UM 4.2.11): 8 gives
 * 1024 Hz. */
#define PIT_MODULUS 8
#define PIT_HZ (REFERENCE_HZ / 4 / PIT_MODULUS)

/* SCI: the fastest rate, SCBR = 1: fsys / (32 * SCBR) = 524288 baud
 * (QSMRM 5.2.1, Equation 5-1). */
#define SCI_SCBR 1

/* Interrupt levels, and IARB values distinct per module (SIM's resets
 * to $F; MC68331UM 4.2.2). */
#define PIT_LEVEL 6
#define GPT_LEVEL 5
#define SCI_LEVEL 4
#define GPT_IARB 0xe
#define QSM_IARB 0xd

/* User vectors, $40 up (CPU32RM 6.1.1, Table 6-1). */
#define PIT_VECTOR 0x40
#define SCI_VECTOR 0x42 /* even: the QSM sends INTV0 zero for the SCI (D.4.3) */
#define GPT_VECTOR_BASE 0x50 /* IVBA $5 */
#define GPT_IC1_VECTOR (GPT_VECTOR_BASE + GPT_SOURCE_IC1)
#define GPT_TOF_VECTOR (GPT_VECTOR_BASE + GPT_SOURCE_TOF)
#define VECTOR_COUNT 256
#define FIRST_USER_VECTOR 0x40

/* The status register: supervisor, interrupt mask at bits 10:8
 * (CPU32RM 2.2, Figure 2-3). */
#define SR_SUPERVISOR 0x2000
#define SR_MASK_ALL 0x0700

/* Exception stack frames (CPU32RM 6.4): SR at the frame's start, the PC
 * at + 2, the format and vector offset word at + 6.  Formats $0, four
 * words; $2, six; $C, the BERR frame, twelve (Figures 6-3, 6-4, 6-6). */
#define FRAME_PC_OFFSET 2
#define FRAME_FORMAT_OFFSET 6
#define FRAME_FORMAT_SHIFT 12
#define FRAME_VECTOR_OFFSET_MASK 0x0fff
#define FORMAT_SIX_WORD 0x2
#define FORMAT_BERR 0xc
#define FRAME_FOUR_WORD_BYTES 8
#define FRAME_SIX_WORD_BYTES 12
#define FRAME_BERR_BYTES 24

#endif
