/*
 * A console demo for a generic MC68331 board (examples/boards/demo.toml):
 * one-letter commands over the SCI, the PIT counting ticks, and the GPT
 * timing a 1 PPS input on IC1 while it toggles OC2.  Integer only, no
 * libgcc: CPU32 has 32-bit MULS.L and DIVU.L (CPU32RM 4).
 */
#include <stdint.h>

#include "board.h"

#define PROMPT "> "
#define LINE_BYTES 32
/* A power of two: the ring indexes wrap by masking. */
#define RX_BUFFER_BYTES 64
#define DUMP_BYTES 64
#define DUMP_ROW_BYTES 16
#define CARRIAGE_RETURN '\r'
#define LINE_FEED '\n'
#define BACKSPACE '\b'
#define DELETE 0x7f
#define PRINTABLE_FIRST ' '
#define PRINTABLE_LAST '~'

/* Mandelbrot in Q12 fixed point over [-2, 0.5] x [-1.125, 1.125]. */
#define FIXED_SHIFT 12
#define FIXED_ONE (1 << FIXED_SHIFT)
#define MANDELBROT_COLUMNS 64
#define MANDELBROT_ROWS 24
#define MANDELBROT_ITERATIONS 32
#define MANDELBROT_LEFT (-2 * FIXED_ONE)
#define MANDELBROT_TOP (-9 * FIXED_ONE / 8)
#define MANDELBROT_X_STEP (5 * FIXED_ONE / 2 / MANDELBROT_COLUMNS)
#define MANDELBROT_Y_STEP (9 * FIXED_ONE / 4 / MANDELBROT_ROWS)
#define MANDELBROT_ESCAPE (4 * FIXED_ONE)
#define MANDELBROT_INSIDE '@'

#define PRIME_LIMIT 1000
#define PRIMES_PER_LINE 12

/* Captures `c` waits for, and how long, in PIT ticks. */
#define PERIOD_CAPTURES 2
#define PERIOD_TIMEOUT_TICKS (3 * PIT_HZ)
/* TIC1 below this with TOF pending was captured after the wrap. */
#define TCNT_HALF 0x8000

/* PIT ticks since reset, PIT_HZ a second. */
volatile uint32_t ticks;

/* Received frames, written by sci_isr at rx_head, read at rx_tail. */
static volatile uint8_t rx_buffer[RX_BUFFER_BYTES];
static volatile uint8_t rx_head;
static volatile uint8_t rx_tail;

/* TCNT extended to 32 bits: overflows counts TOF, the high half. */
static volatile uint32_t overflows;
/* IC1: rising edges captured, the last one's extended TCNT, and the
 * period between the last two in TCNT counts. */
static volatile uint32_t captures;
static volatile uint32_t last_capture;
static volatile uint32_t pps_period;

static uint8_t composite[PRIME_LIMIT];

/* A command, kept out of line so it has a symbol to break on. */
#define COMMAND static __attribute__((noinline))

static const char hex_digits[] = "0123456789abcdef";
static const char shades[] = " .:-=+*#%";

void command_loop(void) __attribute__((noreturn));
void fault_report(const uint16_t *frame);

/* The ISRs: plain C functions, each called by its stub in crt0.S. */
void pit_isr(void);
void sci_isr(void);
void gpt_overflow_isr(void);
void gpt_ic1_isr(void);

void pit_isr(void)
{
	ticks++;
}

/* Reading SCSR then SCDR clears RDRF and OR (MC68331UM D.4.6). */
void sci_isr(void)
{
	uint16_t status = SCSR;
	uint8_t byte = SCDR;
	uint8_t next = (rx_head + 1) & (RX_BUFFER_BYTES - 1);

	if ((status & SCSR_RDRF) && next != rx_tail) {
		rx_buffer[rx_head] = byte;
		rx_head = next;
	}
}

void gpt_overflow_isr(void)
{
	if (TFLG2 & TFLG2_TOF) {
		TFLG2 = (uint8_t)~TFLG2_TOF;
		overflows++;
	}
}

void gpt_ic1_isr(void)
{
	if (!(TFLG1 & TFLG1_IC1F))
		return;
	uint16_t low = TIC1;
	uint32_t high = overflows;
	/* IC1 outranks TOF (GPTRM 7.1, Table 7-1), so an overflow may be
	 * pending, not yet counted.  A capture in TCNT's lower half came
	 * after it. */
	if ((TFLG2 & TFLG2_TOF) && low < TCNT_HALF)
		high++;
	TFLG1 = (uint8_t)~TFLG1_IC1F;
	uint32_t capture = (high << 16) + low;
	if (captures)
		pps_period = capture - last_capture;
	last_capture = capture;
	captures++;
}

static void put_char(char c)
{
	while (!(SCSR & SCSR_TDRE))
		;
	SCDR = (uint8_t)c;
}

static void put_string(const char *s)
{
	for (; *s; s++) {
		if (*s == LINE_FEED)
			put_char(CARRIAGE_RETURN);
		put_char(*s);
	}
}

static void put_hex(uint32_t value, int digits)
{
	while (digits--)
		put_char(hex_digits[(value >> (4 * digits)) & 0xf]);
}

static void put_decimal(uint32_t value)
{
	char digits[10];
	int count = 0;

	do {
		digits[count++] = '0' + value % 10;
		value /= 10;
	} while (value);
	while (count)
		put_char(digits[--count]);
}

/* Right-aligned in `width` columns. */
static void put_decimal_width(uint32_t value, int width)
{
	uint32_t limit = 1;

	while (--width > 0) {
		limit *= 10;
		if (value < limit)
			put_char(' ');
	}
	put_decimal(value);
}

/* Waits in STOP for an interrupt while nothing has come in.  A frame
 * that lands between the test and the STOP waits for the next PIT
 * tick. */
static char get_char(void)
{
	while (rx_tail == rx_head)
		__asm__ volatile("stop %0" : : "i"(SR_SUPERVISOR));
	char c = rx_buffer[rx_tail];
	rx_tail = (rx_tail + 1) & (RX_BUFFER_BYTES - 1);
	return c;
}

/* A line, echoed, ended by CR, LF or CR LF. */
static void read_line(char *line)
{
	static char last;
	int length = 0;

	for (;;) {
		char c = get_char();
		char previous = last;
		last = c;
		if (c == LINE_FEED && previous == CARRIAGE_RETURN)
			continue;
		if (c == CARRIAGE_RETURN || c == LINE_FEED)
			break;
		if ((c == BACKSPACE || c == DELETE) && length) {
			length--;
			put_string("\b \b");
		} else if (c >= PRINTABLE_FIRST && c <= PRINTABLE_LAST
			   && length < LINE_BYTES - 1) {
			line[length++] = c;
			put_char(c);
		}
	}
	line[length] = 0;
	put_string("\n");
}

COMMAND void help(void)
{
	put_string("m       Mandelbrot set\n"
		   "p       primes below 1000\n"
		   "t       uptime from the PIT\n"
		   "c       1 PPS period on IC1, in TCNT counts\n"
		   "x ADDR  dump 64 bytes at hex ADDR\n"
		   "f       address error, reported and recovered\n"
		   "h, ?    this help\n");
}

COMMAND void mandelbrot(void)
{
	for (int row = 0; row < MANDELBROT_ROWS; row++) {
		int32_t cy = MANDELBROT_TOP + row * MANDELBROT_Y_STEP + MANDELBROT_Y_STEP / 2;
		for (int column = 0; column < MANDELBROT_COLUMNS; column++) {
			int32_t cx = MANDELBROT_LEFT + column * MANDELBROT_X_STEP;
			int32_t zr = 0, zi = 0;
			int i;
			for (i = 0; i < MANDELBROT_ITERATIONS; i++) {
				int32_t zr2 = (zr * zr) >> FIXED_SHIFT;
				int32_t zi2 = (zi * zi) >> FIXED_SHIFT;
				if (zr2 + zi2 > MANDELBROT_ESCAPE)
					break;
				/* 2 zr zi: one shift short of Q12. */
				zi = ((zr * zi) >> (FIXED_SHIFT - 1)) + cy;
				zr = zr2 - zi2 + cx;
			}
			int last_shade = sizeof shades - 2;
			put_char(i == MANDELBROT_ITERATIONS ? MANDELBROT_INSIDE
				 : shades[i < last_shade ? i : last_shade]);
		}
		put_string("\n");
	}
}

COMMAND void primes(void)
{
	uint32_t count = 0;

	for (int n = 0; n < PRIME_LIMIT; n++)
		composite[n] = 0;
	for (int n = 2; n < PRIME_LIMIT; n++) {
		if (composite[n])
			continue;
		for (int multiple = n * n; multiple < PRIME_LIMIT; multiple += n)
			composite[multiple] = 1;
		put_decimal_width(n, 4);
		if (++count % PRIMES_PER_LINE == 0)
			put_string("\n");
	}
	put_string("\n");
	put_decimal(count);
	put_string(" primes below ");
	put_decimal(PRIME_LIMIT);
	put_string("\n");
}

COMMAND void uptime(void)
{
	uint32_t now = ticks;
	uint32_t milliseconds = (now % PIT_HZ) * 1000 / PIT_HZ;

	put_string("uptime ");
	put_decimal(now / PIT_HZ);
	put_char('.');
	put_decimal_width(milliseconds, 3);
	put_string(" s (");
	put_decimal(now);
	put_string(" ticks at ");
	put_decimal(PIT_HZ);
	put_string(" Hz)\n");
}

COMMAND void pps(void)
{
	uint32_t start = ticks;

	while (captures < PERIOD_CAPTURES && ticks - start < PERIOD_TIMEOUT_TICKS)
		__asm__ volatile("stop %0" : : "i"(SR_SUPERVISOR));
	if (captures < PERIOD_CAPTURES) {
		put_string("no 1 PPS on IC1\n");
		return;
	}
	uint32_t period = pps_period;
	put_string("1 PPS period ");
	put_decimal(period);
	put_string(" TCNT counts (");
	put_decimal(captures);
	put_string(" captures): system clock ");
	put_decimal(period * TCNT_DIVIDER);
	put_string(" Hz\n");
}

static int parse_hex(const char *s, uint32_t *value)
{
	int digits = 0;

	*value = 0;
	for (; *s; s++, digits++) {
		char c = *s | 0x20; /* ASCII lower case */
		if (c >= '0' && c <= '9')
			*value = (*value << 4) + (c - '0');
		else if (c >= 'a' && c <= 'f')
			*value = (*value << 4) + (c - 'a' + 10);
		else
			return 0;
	}
	return digits > 0;
}

COMMAND void dump(const char *argument)
{
	uint32_t address;

	while (*argument == ' ')
		argument++;
	if (!parse_hex(argument, &address)) {
		put_string("usage: x ADDR\n");
		return;
	}
	for (int row = 0; row < DUMP_BYTES; row += DUMP_ROW_BYTES) {
		const volatile uint8_t *bytes = (const volatile uint8_t *)(address + row);
		put_hex(address + row, 6);
		put_char(' ');
		for (int i = 0; i < DUMP_ROW_BYTES; i++) {
			put_char(' ');
			put_hex(bytes[i], 2);
		}
		put_string("  ");
		for (int i = 0; i < DUMP_ROW_BYTES; i++) {
			uint8_t c = bytes[i];
			put_char(c >= PRINTABLE_FIRST && c <= PRINTABLE_LAST ? c : '.');
		}
		put_string("\n");
	}
}

/* A word read at an odd address: an address error (CPU32RM 6.2.3). */
COMMAND void fault(void)
{
	const volatile uint16_t *odd = (const volatile uint16_t *)(RAM_BASE + 1);

	put_string("reading a word at ");
	put_hex((uint32_t)odd, 6);
	put_string("\n");
	(void)*odd;
	put_string("no address error\n");
}

static uint32_t frame_long(const uint16_t *frame, int offset)
{
	return ((uint32_t)frame[offset / 2] << 16) + frame[offset / 2 + 1];
}

/* Prints an exception's frame, which `frame` points at (CPU32RM 6.4). */
void fault_report(const uint16_t *frame)
{
	uint16_t format = frame[FRAME_FORMAT_OFFSET / 2] >> FRAME_FORMAT_SHIFT;
	uint16_t vector = (frame[FRAME_FORMAT_OFFSET / 2] & FRAME_VECTOR_OFFSET_MASK) / 4;

	put_string("\nexception: vector ");
	put_decimal(vector);
	put_string(", format $");
	put_hex(format, 1);
	put_string(", sr $");
	put_hex(frame[0], 4);
	put_string(", pc $");
	put_hex(frame_long(frame, FRAME_PC_OFFSET), 8);
	put_string("\n");
	if (format == FORMAT_SIX_WORD) {
		/* Figure 6-4. */
		put_string("  faulted instruction $");
		put_hex(frame_long(frame, 8), 8);
		put_string("\n");
	} else if (format == FORMAT_BERR) {
		/* Figure 6-6. */
		put_string("  fault address $");
		put_hex(frame_long(frame, 8), 8);
		put_string(", current instruction $");
		put_hex(frame_long(frame, 16), 8);
		put_string(", ssw $");
		put_hex(frame[11], 4);
		put_string("\n");
	}
}

void command_loop(void)
{
	char line[LINE_BYTES];

	for (;;) {
		put_string(PROMPT);
		read_line(line);
		switch (line[0]) {
		case 0:
			break;
		case 'm':
			mandelbrot();
			break;
		case 'p':
			primes();
			break;
		case 't':
			uptime();
			break;
		case 'c':
			pps();
			break;
		case 'x':
			dump(line + 1);
			break;
		case 'f':
			fault();
			break;
		case 'h':
		case '?':
			help();
			break;
		default:
			put_string("unknown command; h for help\n");
			break;
		}
	}
}

static void sci_init(void)
{
	QSMCR = MCR_SUPV + QSM_IARB;
	QILR = SCI_LEVEL << QILR_ILSCI_SHIFT;
	QIVR = SCI_VECTOR;
	SCCR0 = SCI_SCBR;
	SCCR1 = SCCR1_TE + SCCR1_RE + SCCR1_RIE;
}

static void pit_init(void)
{
	PICR = (PIT_LEVEL << PICR_PIRQL_SHIFT) + PIT_VECTOR;
	PITR = PIT_MODULUS; /* PTP clear */
}

/* IC1 on rising edges, TCNT overflows counted; OC2 toggles each time
 * TCNT passes TOC2, its reset value $FFFF, so every 65536 counts. */
static void gpt_init(void)
{
	GPTMCR = MCR_SUPV + GPT_IARB;
	GPT_ICR = (GPT_LEVEL << ICR_IPL_SHIFT) + ((GPT_VECTOR_BASE >> 4) << ICR_IVBA_SHIFT);
	TCTL1 = TCTL1_OC2_TOGGLE;
	TCTL2 = TCTL2_IC1_RISING;
	TMSK2 = TMSK2_TOI + TMSK2_CPR_DIV4;
	TMSK1 = TMSK1_IC1I;
}

int main(void)
{
	uint8_t reset_status = RSR;

	sci_init();
	pit_init();
	gpt_init();
	__asm__ volatile("move.w %0, %%sr" : : "i"(SR_SUPERVISOR));
	put_string("\nMC68331 demo: system clock ");
	put_decimal(SYSTEM_HZ);
	put_string(" Hz, RSR $");
	put_hex(reset_status, 2);
	put_string("\n");
	help();
	command_loop();
}
