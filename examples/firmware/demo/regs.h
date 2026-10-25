/*
 * MC68331 module registers the demo uses, for C and for crt0.S.
 * Addresses are $YFFxxx with SIMCR MM set, as it is out of reset
 * (MC68331UM D.3.1), so Y = $F.  Register maps: MC68331UM Tables D-2
 * (GPT), D-3 (SIM), D-13 (QSM).
 */
#ifndef DEMO_REGS_H
#define DEMO_REGS_H

/* SIM, MC68331UM D.3. */
#define SYNCR_ADDRESS 0xfffa04   /* D.3.3 */
#define RSR_ADDRESS 0xfffa07     /* D.3.4 */
#define SYPCR_ADDRESS 0xfffa21   /* D.3.12, written once after reset */
#define PICR_ADDRESS 0xfffa22    /* D.3.13 */
#define PITR_ADDRESS 0xfffa24    /* D.3.14 */
#define CSPAR0_ADDRESS 0xfffa44  /* D.3.23 */
#define CSBARBT_ADDRESS 0xfffa48 /* D.3.25 */
#define CSORBT_ADDRESS 0xfffa4a  /* D.3.26 */
#define CSBAR0_ADDRESS 0xfffa4c  /* D.3.25, CSBAR[0:10] */
#define CSOR0_ADDRESS 0xfffa4e   /* D.3.26, CSOR[0:10] */

/* SYNCR fields (D.3.3): fsys = fref * 4 (Y + 1) * 2^(2W + X) (4.3.2). */
#define SYNCR_X (1 << 14)
#define SYNCR_Y_SHIFT 8
#define SYNCR_SLOCK (1 << 3)
#define SYNCR_SLOCK_BIT 3

/* SYPCR bits (D.3.12): SWE, bit 7, clear stops the software watchdog. */
#define SYPCR_HME (1 << 3)
#define SYPCR_BME (1 << 2)

/* PICR PIRQL, bits 10:8, and PIV, bits 7:0 (D.3.13). */
#define PICR_PIRQL_SHIFT 8
/* PITR PITM, bits 7:0; PTP, bit 8, prescales by 512 (D.3.14). */
#define PITR_PTP (1 << 8)

/* CSBARx: ADDR[23:11] in bits 15:3, BLKSZ in bits 2:0 (D.3.25). */
#define CSBAR_ADDRESS_SHIFT 8
#define CSBAR_BLKSZ_64K 0x3 /* Table D-11 */
/* CSORx fields (D.3.26, Table D-12). */
#define CSOR_BYTE_BOTH (0x3 << 13)
#define CSOR_RW_READ (0x1 << 11)
#define CSOR_RW_BOTH (0x3 << 11)
#define CSOR_DSACK_FAST (0xe << 6) /* fast termination */
#define CSOR_SPACE_SU (0x3 << 4)
/* CSPAR0 CSPA0[1], the CS0 field, bits 3:2; 11 a 16-bit port (D.3.23,
 * Table D-10). */
#define CSPAR0_CS0_16BIT (0x3 << 2)

/* QSM, MC68331UM D.4. */
#define QSMCR_ADDRESS 0xfffc00 /* D.4.1 */
#define QILR_ADDRESS 0xfffc04  /* D.4.3 */
#define QIVR_ADDRESS 0xfffc05  /* D.4.3 */
#define SCCR0_ADDRESS 0xfffc08 /* D.4.4 */
#define SCCR1_ADDRESS 0xfffc0a /* D.4.5 */
#define SCSR_ADDRESS 0xfffc0c  /* D.4.6 */
#define SCDR_ADDRESS 0xfffc0e  /* D.4.7 */

/* QILR ILSCI, bits 2:0 of the byte (D.4.3). */
#define QILR_ILSCI_SHIFT 0
/* SCCR1 bits (D.4.5). */
#define SCCR1_RIE (1 << 5)
#define SCCR1_TE (1 << 3)
#define SCCR1_RE (1 << 2)
/* SCSR bits (D.4.6). */
#define SCSR_TDRE (1 << 8)
#define SCSR_RDRF (1 << 6)

/* GPT, MC68331UM D.2. */
#define GPTMCR_ADDRESS 0xfff900 /* D.2.1 */
#define GPT_ICR_ADDRESS 0xfff904 /* D.2.3 */
#define TCNT_ADDRESS 0xfff90a   /* D.2.6 */
#define TIC1_ADDRESS 0xfff90e   /* D.2.8 */
#define TCTL1_ADDRESS 0xfff91e  /* D.2.11 */
#define TCTL2_ADDRESS 0xfff91f  /* D.2.11 */
#define TMSK1_ADDRESS 0xfff920  /* D.2.12 */
#define TMSK2_ADDRESS 0xfff921  /* D.2.12 */
#define TFLG1_ADDRESS 0xfff922  /* D.2.13 */
#define TFLG2_ADDRESS 0xfff923  /* D.2.13 */

/* GPTMCR and QSMCR: SUPV, bit 7, and IARB, bits 3:0, which must be
 * nonzero for the module's interrupts to be serviced (D.2.1, D.4.1). */
#define MCR_SUPV (1 << 7)

/* ICR IPL, bits 10:8, and IVBA, bits 7:4 (D.2.3). */
#define ICR_IPL_SHIFT 8
#define ICR_IVBA_SHIFT 4
/* TCTL1 OM2/OL2, bits 1:0 of the byte: 01 toggles OC2 (D.2.11). */
#define TCTL1_OC2_TOGGLE 0x01
/* TCTL2 EDGE1, bits 1:0 of the byte: 01 captures rising edges (D.2.11). */
#define TCTL2_IC1_RISING 0x01
/* TMSK1 ICI1, bit 0 of the byte (D.2.12). */
#define TMSK1_IC1I (1 << 0)
/* TMSK2 TOI, bit 7 of the byte; CPR, bits 2:0, 000 is system clock / 4,
 * writable once (D.2.12; MC68331UM 7.7; GPTRM Appendix A, CPR). */
#define TMSK2_TOI (1 << 7)
#define TMSK2_CPR_DIV4 0x0
#define TCNT_DIVIDER 4
/* TFLG1 ICF1, bit 0; TFLG2 TOF, bit 7.  A flag clears when read set
 * and then written zero (D.2.13; GPTRM 7.1.2). */
#define TFLG1_IC1F (1 << 0)
#define TFLG2_TOF (1 << 7)
/* GPT interrupt sources, the low nibble of the vector (MC68331UM
 * 7.4.2, Table 7-2). */
#define GPT_SOURCE_IC1 0x1
#define GPT_SOURCE_TOF 0x9

#ifndef __ASSEMBLER__
#include <stdint.h>

#define REG8(address) (*(volatile uint8_t *)(address))
#define REG16(address) (*(volatile uint16_t *)(address))

#define SYNCR REG16(SYNCR_ADDRESS)
#define RSR REG8(RSR_ADDRESS)
#define PICR REG16(PICR_ADDRESS)
#define PITR REG16(PITR_ADDRESS)
#define QSMCR REG16(QSMCR_ADDRESS)
#define QILR REG8(QILR_ADDRESS)
#define QIVR REG8(QIVR_ADDRESS)
#define SCCR0 REG16(SCCR0_ADDRESS)
#define SCCR1 REG16(SCCR1_ADDRESS)
#define SCSR REG16(SCSR_ADDRESS)
#define SCDR REG16(SCDR_ADDRESS)
#define GPTMCR REG16(GPTMCR_ADDRESS)
#define GPT_ICR REG16(GPT_ICR_ADDRESS)
#define TCNT REG16(TCNT_ADDRESS)
#define TIC1 REG16(TIC1_ADDRESS)
#define TCTL1 REG8(TCTL1_ADDRESS)
#define TCTL2 REG8(TCTL2_ADDRESS)
#define TMSK1 REG8(TMSK1_ADDRESS)
#define TMSK2 REG8(TMSK2_ADDRESS)
#define TFLG1 REG8(TFLG1_ADDRESS)
#define TFLG2 REG8(TFLG2_ADDRESS)
#endif

#endif
