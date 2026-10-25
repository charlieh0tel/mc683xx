//! The DUART's register map and bit fields, by the MC68681 datasheet's
//! names (MC68681 Table 4-1, Table 4-5; SCC68692 Table 1, Table 2).

/// Registers, by their number on RS4-RS1 (MC68681 Table 4-1).  Channel
/// B's four are channel A's plus `CHANNEL_B`.
///
/// `MR1x` and `MR2x`, read and written through the mode register pointer.
pub(super) const MR: u8 = 0x0;
/// `SRx` read, `CSRx` written.
pub(super) const SR_CSR: u8 = 0x1;
/// `CRx` written; the read is reserved (MC68681 "Do Not Access",
/// SCC68692 "BRG Test").
pub(super) const CR: u8 = 0x2;
/// `RBx` read (the receive FIFO), `TBx` written (the transmit holding
/// register).
pub(super) const RB_TB: u8 = 0x3;
/// IPCR read, ACR written.
pub(super) const IPCR_ACR: u8 = 0x4;
/// ISR read, IMR written.
pub(super) const ISR_IMR: u8 = 0x5;
/// CUR read, CTUR written.
pub(super) const CUR_CTUR: u8 = 0x6;
/// CLR read, CTLR written.
pub(super) const CLR_CTLR: u8 = 0x7;
/// The first of channel B's registers.
pub(super) const CHANNEL_B: u8 = 0x8;
/// IVR, read and written.
pub(super) const IVR: u8 = 0xc;
/// The input port read, OPCR written.
pub(super) const IP_OPCR: u8 = 0xd;
/// The start-counter command read, the OPR bit set command written.
pub(super) const START_SET: u8 = 0xe;
/// The stop-counter command read, the OPR bit reset command written.
pub(super) const STOP_RESET: u8 = 0xf;
/// RS4-RS1: the peripheral's four address inputs.
pub(super) const REGISTER_MASK: u32 = 0xf;

/// `MR1x` `RxRTS`: the receiver negates RTS while its FIFO is full (MC68681
/// 4.2.1.1).
pub(super) const MR1_RX_RTS: u8 = 0x80;
/// `MR1x` `RxIRQ` select: FFULL rather than `RxRDY` (4.2.1.2).
pub(super) const MR1_RX_INTERRUPT_FFULL: u8 = 0x40;
/// `MR1x` error mode: block rather than character (4.2.1.3).
pub(super) const MR1_ERROR_BLOCK: u8 = 0x20;
/// `MR1x` parity mode, bits 4:3 (4.2.1.4).
pub(super) const MR1_PARITY_SHIFT: u8 = 3;
/// The parity mode field, unshifted.
pub(super) const MR1_PARITY_MASK: u8 = 0b11;
/// Parity mode: with parity.
pub(super) const PARITY_WITH: u8 = 0b00;
/// Parity mode: forced parity.
pub(super) const PARITY_FORCE: u8 = 0b01;
/// Parity mode: multidrop, the parity bit the address/data bit (3.4).
pub(super) const PARITY_MULTIDROP: u8 = 0b11;
/// `MR1x` parity type: odd parity, a forced one, or an address character
/// (4.2.1.5).
pub(super) const MR1_PARITY_TYPE: u8 = 0x04;
/// `MR1x` bits per character, five plus the field (4.2.1.6).
pub(super) const MR1_BITS_MASK: u8 = 0b11;
/// The fewest data bits a character has.
pub(super) const MIN_DATA_BITS: u8 = 5;

/// `MR2x` channel mode, bits 7:6 (4.2.2.1).
pub(super) const MR2_MODE_SHIFT: u8 = 6;
/// `MR2x` `TxRTS`: the transmitter clears OPR's RTS bit after a disable
/// (4.2.2.2).
pub(super) const MR2_TX_RTS: u8 = 0x20;
/// `MR2x` CTS enable: the transmitter waits for CTS low (4.2.2.3).
pub(super) const MR2_CTS: u8 = 0x10;
/// `MR2x` stop bit length, in sixteenths of a bit from 9/16 (4.2.2.4).
pub(super) const MR2_STOP_MASK: u8 = 0x0f;

/// `CSRx`: the receiver's clock select in bits 7:4, the transmitter's in
/// 3:0 (4.2.5).
pub(super) const CSR_RX_SHIFT: u8 = 4;
/// A clock select field.
pub(super) const CSR_FIELD_MASK: u8 = 0x0f;
/// Clock select: the counter/timer's output as a 16X clock.
pub(super) const CSR_TIMER: u8 = 0b1101;

/// `CRx`: enable the receiver (4.2.7.4).
pub(super) const CR_RX_ENABLE: u8 = 0x01;
/// `CRx`: disable the receiver.
pub(super) const CR_RX_DISABLE: u8 = 0x02;
/// `CRx`: enable the transmitter (4.2.7.3).
pub(super) const CR_TX_ENABLE: u8 = 0x04;
/// `CRx`: disable the transmitter.
pub(super) const CR_TX_DISABLE: u8 = 0x08;
/// `CRx`'s miscellaneous command, bits 6:4 on the MC68681 (bit 7 not
/// used, 4.2.7.1), 7:4 on the SCC68692 (SCC68692 CRA\[7:4\]).
pub(super) const CR_COMMAND_SHIFT: u8 = 4;
/// The MC68681's command field, unshifted.
pub(super) const CR_COMMAND_MASK_MC68681: u8 = 0b0111;
/// The SCC68692's.
pub(super) const CR_COMMAND_MASK_SCC68692: u8 = 0b1111;
/// Command: reset the mode register pointer to MR1 (4.2.7.2).
pub(super) const COMMAND_RESET_POINTER: u8 = 0x1;
/// Command: reset the receiver.
pub(super) const COMMAND_RESET_RECEIVER: u8 = 0x2;
/// Command: reset the transmitter.
pub(super) const COMMAND_RESET_TRANSMITTER: u8 = 0x3;
/// Command: reset the error status.
pub(super) const COMMAND_RESET_ERROR: u8 = 0x4;
/// Command: reset the channel's break change interrupt.
pub(super) const COMMAND_RESET_BREAK_CHANGE: u8 = 0x5;
/// Command: start a break.
pub(super) const COMMAND_START_BREAK: u8 = 0x6;
/// Command: stop the break.
pub(super) const COMMAND_STOP_BREAK: u8 = 0x7;
/// SCC68692 command: assert RTSN, setting the channel's OPR bit.
pub(super) const COMMAND_ASSERT_RTS: u8 = 0x8;
/// SCC68692 command: negate RTSN, clearing it.
pub(super) const COMMAND_NEGATE_RTS: u8 = 0x9;
/// SCC68692 command: the receiver's timeout mode on.
pub(super) const COMMAND_TIMEOUT_ON: u8 = 0xa;
/// SCC68692 command: the receiver's timeout mode off.
pub(super) const COMMAND_TIMEOUT_OFF: u8 = 0xc;

/// `SRx` received break (4.2.9.1).
pub(super) const SR_RECEIVED_BREAK: u8 = 0x80;
/// `SRx` framing error (4.2.9.2).
pub(super) const SR_FRAMING_ERROR: u8 = 0x40;
/// `SRx` parity error, or in multidrop mode the address/data bit
/// (4.2.9.3).
pub(super) const SR_PARITY_ERROR: u8 = 0x20;
/// `SRx` overrun error (4.2.9.4).
pub(super) const SR_OVERRUN: u8 = 0x10;
/// `SRx` `TxEMT` (4.2.9.5).
pub(super) const SR_TXEMT: u8 = 0x08;
/// `SRx` `TxRDY` (4.2.9.6).
pub(super) const SR_TXRDY: u8 = 0x04;
/// `SRx` FFULL (4.2.9.7).
pub(super) const SR_FFULL: u8 = 0x02;
/// `SRx` `RxRDY` (4.2.9.8).
pub(super) const SR_RXRDY: u8 = 0x01;
/// The status bits each FIFO entry carries (3.2).
pub(super) const SR_FIFO_STATUS: u8 = SR_RECEIVED_BREAK | SR_FRAMING_ERROR | SR_PARITY_ERROR;

/// ISR and IMR: channel A's `TxRDY` (4.2.15.8).
pub(super) const ISR_TXRDY_A: u8 = 0x01;
/// Channel A's `RxRDY` or FFULL (4.2.15.7).
pub(super) const ISR_RXRDY_A: u8 = 0x02;
/// Channel A's change in break (4.2.15.6).
pub(super) const ISR_BREAK_A: u8 = 0x04;
/// The counter/timer ready (4.2.15.5).
pub(super) const ISR_COUNTER_READY: u8 = 0x08;
/// Channel B's `TxRDY` (4.2.15.4).
pub(super) const ISR_TXRDY_B: u8 = 0x10;
/// Channel B's `RxRDY` or FFULL (4.2.15.3).
pub(super) const ISR_RXRDY_B: u8 = 0x20;
/// The input port change (4.2.15.1).
pub(super) const ISR_INPUT_CHANGE: u8 = 0x80;
/// Channel B's bits are channel A's shifted up.
pub(super) const ISR_CHANNEL_B_SHIFT: u8 = 4;

/// ACR's baud rate set select: set 2 (4.2.13.1).
pub(super) const ACR_BRG_SET_2: u8 = 0x80;
/// ACR's counter/timer mode and source, bits 6:4 (4.2.13.2, Table
/// 4-4).
pub(super) const ACR_COUNTER_SHIFT: u8 = 4;
/// The field, unshifted.
pub(super) const ACR_COUNTER_MASK: u8 = 0b111;
/// ACR\[6\]: timer mode rather than counter mode.
pub(super) const ACR_TIMER: u8 = 0x40;
/// ACR's change-of-state interrupt enables for IP3-IP0 (4.2.13.3).
pub(super) const ACR_DELTA_MASK: u8 = 0x0f;

/// Counter/timer source, ACR\[6:4\] (Table 4-4), the IP2 sources left
/// out: counter, channel A's transmitter 1X clock.
pub(super) const SOURCE_COUNTER_TXCA: u8 = 0b001;
/// Counter, channel B's transmitter 1X clock.
pub(super) const SOURCE_COUNTER_TXCB: u8 = 0b010;
/// Counter, X1/CLK divided by 16.
pub(super) const SOURCE_COUNTER_X1_16: u8 = 0b011;
/// Timer, X1/CLK.
pub(super) const SOURCE_TIMER_X1: u8 = 0b110;
/// Timer, X1/CLK divided by 16.
pub(super) const SOURCE_TIMER_X1_16: u8 = 0b111;
/// The prescaler of the divided sources.
pub(super) const SOURCE_PRESCALE: u64 = 16;

/// IPCR's change-of-state bits for IP3-IP0, bits 7:4 (4.2.14.1).
pub(super) const IPCR_DELTA_SHIFT: u8 = 4;
/// The input pins with change-of-state detectors, IP3-IP0 (1.6).
pub(super) const DETECTED_PINS: usize = 4;

/// OPCR: OP7 is `TxRDYB`'s interrupt output (4.2.11.1).
pub(super) const OPCR_OP7_TXRDY_B: u8 = 0x80;
/// OP6 is `TxRDYA`'s (4.2.11.2).
pub(super) const OPCR_OP6_TXRDY_A: u8 = 0x40;
/// OP5 is channel B's receiver interrupt (4.2.11.3).
pub(super) const OPCR_OP5_RXRDY_B: u8 = 0x20;
/// OP4 is channel A's (4.2.11.4).
pub(super) const OPCR_OP4_RXRDY_A: u8 = 0x10;
/// OPCR\[3:2\], OP3's function (4.2.11.5).
pub(super) const OPCR_OP3_SHIFT: u8 = 2;
/// A two-bit OPCR field, unshifted.
pub(super) const OPCR_FIELD_MASK: u8 = 0b11;
/// OP3 is the counter/timer output.
pub(super) const OP3_COUNTER: u8 = 0b01;
/// OPCR\[1:0\] zero: OP2 is OPR\[2\] (4.2.11.6).
pub(super) const OP2_GENERAL: u8 = 0b00;

/// The output port pins, OP0-OP7 (1.7).
pub(super) const OUTPUTS: usize = 8;
/// The input port pins, IP0-IP5 (1.6).
pub(super) const INPUTS: usize = 6;
/// The input port read: IP5-IP0 in bits 5:0 (Table 4-5, INPUT PORT).
pub(super) const INPUT_PINS_MASK: u8 = 0x3f;
/// Bit 7 of the input port reads one (Table 4-5, INPUT PORT).
pub(super) const INPUT_BIT_7: u8 = 0x80;
/// Bit 6: IACK's level (Table 4-5, INPUT PORT; SCC68692 1990, Input
/// Port).
pub(super) const INPUT_BIT_6: u8 = 0x40;
/// The CTS inputs: IP0 channel A's, IP1 channel B's (2.16.1, 2.16.2).
pub(super) const CTS_PINS: [usize; 2] = [0, 1];
/// The RTS outputs: OP0 channel A's, OP1 channel B's (2.17.1, 2.17.2).
pub(super) const RTS_PINS: [usize; 2] = [0, 1];
/// The counter/timer's output pin.
pub(super) const OP3: usize = 3;
/// The pins that can be interrupt outputs, OP4-OP7, by their OPCR bit,
/// and the ISR bit each shows the complement of (4.2.11.1-4.2.11.4).
pub(super) const INTERRUPT_OUTPUTS: [(usize, u8, u8); 4] = [
    (4, OPCR_OP4_RXRDY_A, ISR_RXRDY_A),
    (5, OPCR_OP5_RXRDY_B, ISR_RXRDY_B),
    (6, OPCR_OP6_TXRDY_A, ISR_TXRDY_A),
    (7, OPCR_OP7_TXRDY_B, ISR_TXRDY_B),
];

/// IVR after a reset: the M68000's uninitialized interrupt vector
/// (4.2.19).
pub(super) const IVR_RESET: u8 = 0x0f;
/// What a read of a command address or a reserved register gives: the
/// datasheets do not say; ours.
pub(super) const UNDRIVEN: u8 = 0xff;
/// The receive FIFO's holding registers (3.2).
pub(super) const FIFO_DEPTH: usize = 3;
/// The crystal the datasheets give the baud rates for (1.2; Table
/// 4-6).
pub(super) const X1_DEFAULT_HZ: u64 = 3_686_400;
/// The change-of-state detectors' sampling clock, X1/CLK / 96: 38.4 kHz
/// from a 3.6864 MHz crystal (1.6).
pub(super) const SAMPLE_DIVISOR: u64 = 96;
/// The least preload the counter/timer takes, CTUR:CTLR (4.2.18).
pub(super) const PRELOAD_MINIMUM: u32 = 2;
/// A preload of zero counts this many: the 16-bit counter's full turn,
/// our reading where the datasheets set a minimum of 2.
pub(super) const COUNTER_TURN: u32 = 0x1_0000;

/// Sixteenths of a bit: the 16X clock's periods in one bit time
/// (1.2).
pub(super) const SIXTEENTHS: u64 = 16;

/// The baud rate generator's 16X clock divisors of X1/CLK for CSR codes
/// 0 to 12, baud rate set 1 and set 2 (Table 4-5, CSRA; ACR\[7\]).
/// Each is X1/CLK over the 16X clock Table 4-6 gives for its rate at
/// 3.6864 MHz: 50 baud is a 0.8 kHz clock, 4608; 110 baud 1.759 kHz,
/// 2096; 134.5 baud 2.153 kHz, 1712; 1050 baud 16.756 kHz, 220; 2000
/// baud 32.056 kHz, 115.
pub(super) const BRG_SET_1: [u64; 13] = [
    4608, // 50
    2096, // 110
    1712, // 134.5
    1152, // 200
    768,  // 300
    384,  // 600
    192,  // 1200
    220,  // 1050
    96,   // 2400
    48,   // 4800
    32,   // 7200
    24,   // 9600
    6,    // 38.4k
];
/// Set 2's.
pub(super) const BRG_SET_2: [u64; 13] = [
    3072, // 75
    2096, // 110
    1712, // 134.5
    1536, // 150
    768,  // 300
    384,  // 600
    192,  // 1200
    115,  // 2000
    96,   // 2400
    48,   // 4800
    128,  // 1800
    24,   // 9600
    12,   // 19.2k
];
