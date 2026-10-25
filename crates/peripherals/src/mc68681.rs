//! The Motorola MC68681 and Philips SCC68692 DUART (MC68681 datasheet,
//! cited by section; SCC68692 datasheet, by heading), registered as
//! `mc68681` and `scc68692`: two serial channels, a counter/timer, a
//! 6-bit input port and an 8-bit output port, on the bus through RS1-RS4
//! and D7-D0 with a vectored interrupt.
//!
//! The board reaches it only through its attachments and ports
//! ([docs/peripherals.md](https://github.com/charlieh0tel/mc683xx/blob/main/docs/peripherals.md)): `RxDA`, `TxDA`, `RxDB` and `TxDB` are serial nets (an
//! `external` endpoint, so `--bind` and Python's `send` reach a
//! channel), IP0-IP5 logic inputs, OP0-OP7 logic outputs at the level
//! each pin shows, and IRQ the interrupt output's level; a peripheral `irq`
//! wires the same output to an IRQ pin.  An acknowledge through an
//! `iack` attachment reads the IVR while the interrupt is asserted.
//! X1/CLK is the parameter `x1_hz`, 3.6864 MHz by default.
//!
//! The two peripherals differ where a board file must say which it has: the
//! SCC68692's command register takes four bits of miscellaneous
//! command, with RTS, timeout mode and power down commands where the
//! MC68681 ignores bit 7; and its RESET stops the counter/timer until
//! a start command, where the MC68681's puts it in timer mode, running.
//! Both show IACK's level in
//! input port bit 6 ([docs/peripherals.md](https://github.com/charlieh0tel/mc683xx/blob/main/docs/peripherals.md),
//! "Input port").

mod channel;
mod counter;
mod registers;
#[cfg(test)]
mod tests;
mod timing;

use mc683xx_board::inspect::Inspection;
use mc683xx_board::inspect::Radix;
use mc683xx_board::peripheral::Drive;
use mc683xx_board::peripheral::Peripheral;
use mc683xx_board::peripheral::PeripheralAccess;
use mc683xx_board::peripheral::Role;
use mc683xx_board::registry::Context;
use mc683xx_board::registry::ParameterError;
use mc683xx_board::registry::PortSpec;
use mc683xx_board::registry::Registry;
use mc683xx_board::registry::RegistryError;
use mc683xx_board::signal::Kind;
use mc683xx_board::signal::Serial;
use mc683xx_board::signal::Signal;
use mc683xx_board::time::Time;
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use serde::Deserialize;
use serde::Serialize;

use self::channel::Channel;
use self::channel::Clocks;
use self::counter::Counter;
use self::counter::Source;
use self::registers::ACR_BRG_SET_2;
use self::registers::ACR_COUNTER_MASK;
use self::registers::ACR_COUNTER_SHIFT;
use self::registers::ACR_DELTA_MASK;
use self::registers::ACR_TIMER;
use self::registers::CHANNEL_B;
use self::registers::CLR_CTLR;
use self::registers::COMMAND_ASSERT_RTS;
use self::registers::COMMAND_NEGATE_RTS;
use self::registers::COMMAND_TIMEOUT_OFF;
use self::registers::COMMAND_TIMEOUT_ON;
use self::registers::CR;
use self::registers::CR_COMMAND_MASK_MC68681;
use self::registers::CR_COMMAND_MASK_SCC68692;
use self::registers::CR_COMMAND_SHIFT;
use self::registers::CTS_PINS;
use self::registers::CUR_CTUR;
use self::registers::DETECTED_PINS;
use self::registers::INPUT_BIT_6;
use self::registers::INPUT_BIT_7;
use self::registers::INPUT_PINS_MASK;
use self::registers::INPUTS;
use self::registers::INTERRUPT_OUTPUTS;
use self::registers::IP_OPCR;
use self::registers::IPCR_ACR;
use self::registers::IPCR_DELTA_SHIFT;
use self::registers::ISR_BREAK_A;
use self::registers::ISR_CHANNEL_B_SHIFT;
use self::registers::ISR_COUNTER_READY;
use self::registers::ISR_IMR;
use self::registers::ISR_INPUT_CHANGE;
use self::registers::ISR_RXRDY_A;
use self::registers::ISR_TXRDY_A;
use self::registers::IVR;
use self::registers::IVR_RESET;
use self::registers::MR;
use self::registers::OP2_GENERAL;
use self::registers::OP3;
use self::registers::OP3_COUNTER;
use self::registers::OPCR_FIELD_MASK;
use self::registers::OPCR_OP3_SHIFT;
use self::registers::OUTPUTS;
use self::registers::RB_TB;
use self::registers::REGISTER_MASK;
use self::registers::RTS_PINS;
use self::registers::SAMPLE_DIVISOR;
use self::registers::SIXTEENTHS;
use self::registers::SOURCE_COUNTER_TXCA;
use self::registers::SOURCE_COUNTER_TXCB;
use self::registers::SOURCE_COUNTER_X1_16;
use self::registers::SOURCE_PRESCALE;
use self::registers::SOURCE_TIMER_X1;
use self::registers::SOURCE_TIMER_X1_16;
use self::registers::SR_CSR;
use self::registers::START_SET;
use self::registers::STOP_RESET;
use self::registers::UNDRIVEN;
use self::registers::X1_DEFAULT_HZ;
use self::timing::Crystal;

/// The MC68681's registered name.
pub(crate) const MC68681: &str = "mc68681";
/// The SCC68692's.
pub(crate) const SCC68692: &str = "scc68692";
/// The parameter giving X1/CLK's frequency in hertz.
const X1_HZ: &str = "x1_hz";
/// The format of the DUART's snapshots.
const VERSION: u32 = 2;

/// The ports, by index: the channels' serial lines, IP0-IP5, OP0-OP7,
/// then IRQ.
const RXDA: usize = 0;
/// `TxDA`.
const TXDA: usize = 1;
/// `RxDB`.
const RXDB: usize = 2;
/// `TxDB`.
const TXDB: usize = 3;
/// IP0; IP1-IP5 follow.
const IP0: usize = 4;
/// OP0; OP1-OP7 follow.
const OP0: usize = IP0 + INPUTS;
/// IRQ (MC68681 2.10; the SCC68692's INTRN).
const IRQ: usize = OP0 + OUTPUTS;
/// The bit of IRQ in the levels last driven, after OP0-OP7's.
const IRQ_BIT: u16 = 1 << OUTPUTS;

/// `mask` if `set`, else none of it.
const fn bits(set: bool, mask: u8) -> u8 {
    if set { mask } else { 0 }
}

/// Which peripheral it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Variant {
    /// The Motorola MC68681.
    Mc68681,
    /// The Philips SCC68692.
    Scc68692,
}

/// Registers `mc68681`.
pub(crate) fn register_mc68681(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register(MC68681, ports(), |context| {
        Ok(Box::new(Duart::from_parameters(context, Variant::Mc68681)?))
    })
}

/// Registers `scc68692`.
pub(crate) fn register_scc68692(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register(SCC68692, ports(), |context| {
        Ok(Box::new(Duart::from_parameters(
            context,
            Variant::Scc68692,
        )?))
    })
}

/// The ports, in index order, by the MC68681's pin names (Table 2-1).
fn ports() -> Vec<PortSpec> {
    let mut ports = vec![
        PortSpec::input("RxDA", Kind::Serial),
        PortSpec::output("TxDA", Kind::Serial),
        PortSpec::input("RxDB", Kind::Serial),
        PortSpec::output("TxDB", Kind::Serial),
    ];
    ports.extend((0..INPUTS).map(|pin| PortSpec::input(format!("IP{pin}"), Kind::Logic)));
    ports.extend((0..OUTPUTS).map(|pin| PortSpec::output(format!("OP{pin}"), Kind::Logic)));
    ports.push(PortSpec::output("IRQ", Kind::Logic));
    ports
}

/// The DUART's state: what its snapshot holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct State {
    /// Channels A and B.
    channels: [Channel; 2],
    /// ACR (4.2.13).
    acr: u8,
    /// IMR (4.2.16).
    imr: u8,
    /// IVR (4.2.19).
    ivr: u8,
    /// OPCR (4.2.11).
    opcr: u8,
    /// OPR (4.2.12).
    opr: u8,
    /// The counter/timer.
    counter: Counter,
    /// The channel in the SCC68692's timeout mode, if one is (SCC68692,
    /// Timeout Mode).
    timeout: Option<usize>,
    /// The levels on IP0-IP5, bit 0 IP0; high until driven.
    inputs: u8,
    /// The levels on IP3-IP0 the change-of-state detectors have
    /// recognized (1.6).
    recognized: u8,
    /// When each of IP3-IP0's change to its present level is recognized,
    /// while one is pending.
    pending: [Option<Time>; DETECTED_PINS],
    /// IPCR\[7:4\], the changes of state since IPCR was read, here in bits
    /// 3:0.
    deltas: u8,
    /// ISR\[7\], the input port change (4.2.15.1).
    input_change: bool,
    /// The time it was last brought to.
    now: Time,
    /// The levels last driven on OP0-OP7 and IRQ (`IRQ_BIT`), none
    /// before the first drive.
    driven: Option<u16>,
}

impl State {
    /// Refuses what no run of `variant` holds: a channel or the counter
    /// their checks refuse, timeout mode on the MC68681 (which has no
    /// such command) or on a third channel, an input port bit past IP5,
    /// a change of state past IP3 or one pending that is not, or levels
    /// past IRQ's.
    fn check(&self, variant: Variant) -> Result<(), StateError> {
        for channel in &self.channels {
            channel.check()?;
        }
        self.counter.check()?;
        let invalid = |what| Err(StateError::invalid(what));
        if self
            .timeout
            .is_some_and(|index| index >= self.channels.len() || variant == Variant::Mc68681)
        {
            return invalid("a DUART timeout mode its peripheral does not have");
        }
        if (self.inputs | self.recognized) & !INPUT_PINS_MASK != 0
            || self.deltas >> DETECTED_PINS != 0
        {
            return invalid("a DUART input port bit past its pins");
        }
        let changing = self.inputs ^ self.recognized;
        if (0..DETECTED_PINS).any(|pin| self.pending[pin].is_some() != (changing & 1 << pin != 0)) {
            return invalid("a DUART change of state pending without a change");
        }
        if self
            .driven
            .is_some_and(|driven| driven >> (OUTPUTS + 1) != 0)
        {
            return invalid("a DUART output past IRQ");
        }
        Ok(())
    }
}

impl Default for State {
    fn default() -> Self {
        Self {
            channels: [Channel::default(), Channel::default()],
            acr: ACR_TIMER,
            imr: 0,
            ivr: IVR_RESET,
            opcr: 0,
            opr: 0,
            counter: Counter::default(),
            timeout: None,
            inputs: INPUT_PINS_MASK,
            recognized: INPUT_PINS_MASK,
            pending: [None; DETECTED_PINS],
            deltas: 0,
            input_change: false,
            now: Time::ZERO,
            driven: None,
        }
    }
}

/// A DUART on the board.
#[derive(Debug)]
struct Duart {
    /// Which peripheral.
    variant: Variant,
    /// X1/CLK.
    crystal: Crystal,
    /// Its state.
    state: State,
    /// Frames to drive on `TxDA` and `TxDB`: their start, port and frame.
    transmitted: Vec<(Time, usize, Serial)>,
}

impl Duart {
    /// A DUART out of reset, from `x1_hz`, X1/CLK's frequency.
    fn from_parameters(context: &Context<'_>, variant: Variant) -> Result<Self, ParameterError> {
        let hz = context.parameters.integer(X1_HZ)?.unwrap_or(X1_DEFAULT_HZ);
        if hz == 0 {
            return Err(ParameterError::invalid(X1_HZ, "X1/CLK must run"));
        }
        Ok(Self::new(variant, hz))
    }

    /// A DUART out of reset, X1/CLK at `hz`.
    fn new(variant: Variant, hz: u64) -> Self {
        Self {
            variant,
            crystal: Crystal { hz },
            state: State::default(),
            transmitted: Vec::new(),
        }
    }

    /// The counter/timer's mode and source (Table 4-4): in the
    /// SCC68692's timeout mode a counter whatever ACR\[6\] says.
    fn source(&self) -> Source {
        let state = &self.state;
        let field = state.acr >> ACR_COUNTER_SHIFT & ACR_COUNTER_MASK;
        let timer = state.acr & ACR_TIMER != 0 && state.timeout.is_none();
        let field = if timer {
            field
        } else {
            field & !(ACR_TIMER >> ACR_COUNTER_SHIFT)
        };
        let generator = Clocks {
            crystal: self.crystal,
            set_2: state.acr & ACR_BRG_SET_2 != 0,
            timer: None,
        };
        let transmitter =
            |channel: &Channel| Some(SIXTEENTHS * channel.transmit_divisor(&generator)?);
        let period = match field {
            SOURCE_COUNTER_TXCA => transmitter(&state.channels[0]),
            SOURCE_COUNTER_TXCB => transmitter(&state.channels[1]),
            SOURCE_COUNTER_X1_16 | SOURCE_TIMER_X1_16 => Some(SOURCE_PRESCALE),
            SOURCE_TIMER_X1 => Some(1),
            // IP2, divided or not: no clock on a logic net
            _ => None,
        };
        Source { timer, period }
    }

    /// The clocks the channels select from.
    fn clocks(&self) -> Clocks {
        Clocks {
            crystal: self.crystal,
            set_2: self.state.acr & ACR_BRG_SET_2 != 0,
            timer: self.state.counter.clock_divisor(self.source()),
        }
    }

    /// Whether channel `channel`'s CTS input, IP0 or IP1, is asserted
    /// (low).
    const fn cts(&self, channel: usize) -> bool {
        self.state.inputs & 1 << CTS_PINS[channel] == 0
    }

    /// ISR (4.2.15).
    fn interrupt_status(&self) -> u8 {
        let [a, b] = &self.state.channels;
        let channel = |channel: &Channel| {
            [
                (channel.transmitter_ready(), ISR_TXRDY_A),
                (channel.receiver_ready(), ISR_RXRDY_A),
                (channel.break_change(), ISR_BREAK_A),
            ]
            .into_iter()
            .fold(0, |isr, (set, bit)| isr | bits(set, bit))
        };
        channel(a)
            | (channel(b) << ISR_CHANNEL_B_SHIFT)
            | bits(self.state.counter.ready, ISR_COUNTER_READY)
            | bits(self.state.input_change, ISR_INPUT_CHANGE)
    }

    /// Whether IRQ is asserted: an ISR bit the IMR passes (4.2.16).
    fn interrupting(&self) -> bool {
        self.interrupt_status() & self.state.imr != 0
    }

    /// The level each output pin shows, `None` for a clock output the
    /// model does not drive (OPCR, 4.2.11): the complement of its OPR bit
    /// (1.7), or an open-drain output released high and pulled low.
    fn output_levels(&self) -> [Option<bool>; OUTPUTS] {
        let state = &self.state;
        let general = |pin: usize| state.opr & 1 << pin == 0;
        let isr = self.interrupt_status();
        let mut levels = [None; OUTPUTS];
        for (channel, &pin) in RTS_PINS.iter().enumerate() {
            levels[pin] = Some(general(pin) || state.channels[channel].rts_negated());
        }
        levels[2] = (state.opcr & OPCR_FIELD_MASK == OP2_GENERAL).then(|| general(2));
        levels[OP3] = match state.opcr >> OPCR_OP3_SHIFT & OPCR_FIELD_MASK {
            OP2_GENERAL => Some(general(OP3)),
            OP3_COUNTER => Some(state.counter.output),
            _ => None,
        };
        for (pin, select, bit) in INTERRUPT_OUTPUTS {
            levels[pin] = Some(if state.opcr & select != 0 {
                isr & bit == 0
            } else {
                general(pin)
            });
        }
        levels
    }

    /// The input port read (Table 4-5, INPUT PORT): bit 7 one, bit 6
    /// IACK's level, high, since a register read is no acknowledge.
    fn input_port(&self) -> u8 {
        INPUT_BIT_7 | INPUT_BIT_6 | self.state.inputs & INPUT_PINS_MASK
    }

    /// When the DUART next changes on its own, but for the counter/timer.
    fn next_internal(&self) -> Option<Time> {
        let clocks = self.clocks();
        let channels = self
            .state
            .channels
            .iter()
            .enumerate()
            .filter_map(|(index, channel)| channel.next_event(&clocks, self.cts(index)));
        channels
            .chain(self.state.pending.iter().flatten().copied())
            .min()
    }

    /// Brings the DUART to `now`, each event at its own time.
    fn run_until(&mut self, now: Time) {
        let now = now.max(self.state.now);
        while let Some(at) = self.next_internal().filter(|&at| at <= now) {
            self.step(at);
        }
        let source = self.source();
        self.state.counter.catch_up(self.crystal.tick(now), source);
        self.state.now = now;
    }

    /// Everything due at `at`.
    fn step(&mut self, at: Time) {
        let clocks = self.clocks();
        for index in 0..self.state.channels.len() {
            let cts = self.cts(index);
            let channel = &mut self.state.channels[index];
            if channel
                .next_event(&clocks, cts)
                .is_some_and(|next| next <= at)
            {
                channel.step(at, &clocks, cts);
            }
            self.collect(index);
        }
        for pin in 0..DETECTED_PINS {
            if self.state.pending[pin].is_some_and(|recognized| recognized <= at) {
                self.recognize(pin);
            }
        }
    }

    /// Takes what channel `index` did that reaches outside it.
    fn collect(&mut self, index: usize) {
        let effects = self.state.channels[index].take_effects();
        let port = [TXDA, TXDB][index];
        self.transmitted.extend(
            effects
                .transmitted
                .into_iter()
                .map(|(at, serial)| (at, port, serial)),
        );
        if effects.clear_rts {
            self.state.opr &= !(1 << RTS_PINS[index]);
        }
        if let Some(at) = effects.transferred
            && self.state.timeout == Some(index)
        {
            // timeout mode: each character into the FIFO restarts the
            // counter, clearing ISR[3] (SCC68692, Timeout Mode)
            let source = self.source();
            let tick = self.crystal.tick(at);
            self.state.counter.start(tick, source);
            self.state.counter.ready = false;
        }
    }

    /// IP`pin`'s change to its present level is recognized: IPCR's delta,
    /// and ISR\[7\] if ACR enables it (4.2.13.3, 4.2.14.1).
    fn recognize(&mut self, pin: usize) {
        let state = &mut self.state;
        let bit = 1 << pin;
        state.pending[pin] = None;
        state.recognized = state.recognized & !bit | state.inputs & bit;
        state.deltas |= bit;
        if state.acr & ACR_DELTA_MASK & bit != 0 {
            state.input_change = true;
        }
    }

    /// IP`pin` driven `high` at `now`.  A change on IP3-IP0 is recognized
    /// after two samples of the 38.4 kHz clock at the new level, 25 to 50
    /// microseconds; one shorter is not (1.6).
    fn set_input(&mut self, pin: usize, high: bool, now: Time) {
        let bit = 1 << pin;
        let state = &mut self.state;
        state.inputs = if high {
            state.inputs | bit
        } else {
            state.inputs & !bit
        };
        if pin < DETECTED_PINS {
            state.pending[pin] = (state.recognized & bit != state.inputs & bit).then(|| {
                let first = self.crystal.tick(now).next_multiple_of(SAMPLE_DIVISOR);
                self.crystal.time(first + SAMPLE_DIVISOR)
            });
        }
        let clocks = self.clocks();
        for index in 0..self.state.channels.len() {
            let cts = self.cts(index);
            self.state.channels[index].try_transmit(now, &clocks, cts);
            self.collect(index);
        }
    }

    /// The channel a register belongs to and its number among the
    /// channel's four, for channel registers.
    fn channel_register(register: u8) -> Option<(usize, u8)> {
        let channel = usize::from(register & CHANNEL_B != 0);
        let local = register & !CHANNEL_B;
        (register < IVR && local < IPCR_ACR).then_some((channel, local))
    }

    /// Register `register` read at `now`; some reads act.
    fn read_register(&mut self, register: u8, now: Time) -> u8 {
        let tick = self.crystal.tick(now);
        let source = self.source();
        if let Some((index, local)) = Self::channel_register(register) {
            let channel = &mut self.state.channels[index];
            let value = match local {
                MR => channel.read_mode(),
                SR_CSR => channel.status(),
                RB_TB => channel.read_data(now),
                _ => UNDRIVEN,
            };
            self.collect(index);
            return value;
        }
        let state = &mut self.state;
        match register {
            IPCR_ACR => {
                let value = state.deltas << IPCR_DELTA_SHIFT | state.inputs & ACR_DELTA_MASK;
                state.deltas = 0;
                state.input_change = false;
                value
            }
            ISR_IMR => self.interrupt_status(),
            CUR_CTUR => state.counter.value(tick, source).to_be_bytes()[0],
            CLR_CTLR => state.counter.value(tick, source).to_be_bytes()[1],
            IVR => state.ivr,
            IP_OPCR => self.input_port(),
            START_SET => {
                if state.timeout.is_none() {
                    state.counter.start(tick, source);
                }
                UNDRIVEN
            }
            STOP_RESET => {
                if state.timeout.is_none() {
                    state.counter.stop(tick, source);
                }
                UNDRIVEN
            }
            _ => UNDRIVEN,
        }
    }

    /// Register `register` written `value` at `now`.
    fn write_register(&mut self, register: u8, value: u8, now: Time) {
        let tick = self.crystal.tick(now);
        let clocks = self.clocks();
        if let Some((index, local)) = Self::channel_register(register) {
            let cts = self.cts(index);
            match local {
                MR => self.state.channels[index].write_mode(value),
                SR_CSR => {
                    // a transmitter's clock may be the counter's source
                    let old = self.source();
                    self.state.channels[index].write_clock_select(value);
                    self.state.counter.rebase(tick, old);
                }
                CR => self.command(index, value, now),
                _ => self.state.channels[index].write_data(value, now, &clocks, cts),
            }
            self.collect(index);
            return;
        }
        let state = &mut self.state;
        match register {
            IPCR_ACR => {
                let old = self.source();
                self.state.acr = value;
                self.state.counter.rebase(tick, old);
            }
            ISR_IMR => state.imr = value,
            CUR_CTUR => {
                state.counter.preload = state.counter.preload & 0x00ff | u16::from(value) << 8;
            }
            CLR_CTLR => state.counter.preload = state.counter.preload & 0xff00 | u16::from(value),
            IVR => state.ivr = value,
            IP_OPCR => state.opcr = value,
            START_SET => state.opr |= value,
            STOP_RESET => state.opr &= !value,
            _ => {}
        }
    }

    /// CR`x` of channel `index` written `value` at `now` (4.2.7).
    fn command(&mut self, index: usize, value: u8, now: Time) {
        let mask = match self.variant {
            Variant::Mc68681 => CR_COMMAND_MASK_MC68681,
            Variant::Scc68692 => CR_COMMAND_MASK_SCC68692,
        };
        let command = value >> CR_COMMAND_SHIFT & mask;
        let rts = 1 << RTS_PINS[index];
        let tick = self.crystal.tick(now);
        match command {
            COMMAND_ASSERT_RTS => self.state.opr |= rts,
            COMMAND_NEGATE_RTS => self.state.opr &= !rts,
            COMMAND_TIMEOUT_ON => {
                // the counter stopped until the next character, ISR[3]
                // clear
                let source = self.source();
                self.state.counter.catch_up(tick, source);
                self.state.timeout = Some(index);
                let source = self.source();
                self.state.counter.stop(tick, source);
            }
            COMMAND_TIMEOUT_OFF => {
                let old = self.source();
                self.state.timeout = None;
                self.state.counter.rebase(tick, old);
            }
            _ => {}
        }
        let clocks = self.clocks();
        let cts = self.cts(index);
        self.state.channels[index].command(value, command, now, &clocks, cts);
    }

    /// RESET at `now` (2.4; SCC68692 RESETN): SRA, SRB, IMR, ISR, OPR
    /// and OPCR clear, the IVR $0F, the output port high, the channels
    /// inactive and their mode register pointers at MR1.  The
    /// SCC68692's counter/timer stops until a start command (RESETN;
    /// Counter/Timer); the MC68681's is in timer mode, which "runs
    /// continuously and cannot be started or stopped" (2.4, 3.5), on
    /// the preload it has.  The modes, clock selects, ACR's other bits
    /// and the preload stay.
    fn hardware_reset(&mut self, now: Time) {
        let state = &mut self.state;
        for channel in &mut state.channels {
            channel.reset();
        }
        state.imr = 0;
        state.opr = 0;
        state.opcr = 0;
        state.ivr = IVR_RESET;
        state.counter.reset();
        state.timeout = None;
        state.input_change = false;
        if self.variant == Variant::Mc68681 {
            state.acr |= ACR_TIMER;
            state.counter.run_from(self.crystal.tick(now));
        }
    }

    /// Drives what changed: frames started on `TxDA` and `TxDB`, at their
    /// start bits, and the output pins and IRQ that changed level, now.
    fn sync(&mut self, drive: &mut Drive<'_>) {
        for (at, port, serial) in self.transmitted.drain(..) {
            drive.drive_at(at, port, Signal::Serial(serial));
        }
        let driven = self.state.driven;
        let mut levels = driven.unwrap_or(0);
        for (pin, level) in self.output_levels().into_iter().enumerate() {
            if let Some(high) = level.or_else(|| driven.is_none().then_some(true)) {
                levels = (levels & !(1 << pin)) | (u16::from(high) << pin);
            }
        }
        levels = (levels & !IRQ_BIT) | (u16::from(!self.interrupting()) << OUTPUTS);
        let changed = driven.map_or(u16::MAX, |driven| driven ^ levels);
        for bit in 0..=OUTPUTS {
            if changed & 1 << bit != 0 {
                let port = if bit == OUTPUTS { IRQ } else { OP0 + bit };
                drive.drive(port, Signal::Level(levels & 1 << bit != 0));
            }
        }
        self.state.driven = Some(levels);
    }

    /// The register an access reaches: RS4-RS1 on the peripheral's address
    /// lines.
    const fn register(access: &PeripheralAccess) -> u8 {
        (access.address & REGISTER_MASK) as u8
    }
}

impl Peripheral for Duart {
    fn read(&mut self, access: PeripheralAccess, drive: &mut Drive<'_>) -> Result<u32, BusError> {
        self.run_until(access.now);
        let value = if access.role == Role::Iack {
            // the IVR while IRQ is asserted; otherwise the DUART ignores
            // IACK and nothing ends the cycle (2.11)
            self.interrupting()
                .then_some(u32::from(self.state.ivr))
                .ok_or(BusError)
        } else {
            Ok(access.read_byte(self.read_register(Self::register(&access), access.now)))
        };
        self.sync(drive);
        value
    }

    fn write(
        &mut self,
        access: PeripheralAccess,
        value: u32,
        drive: &mut Drive<'_>,
    ) -> Result<(), BusError> {
        self.run_until(access.now);
        if access.role == Role::Iack {
            return Err(BusError);
        }
        let value = access.written_byte(value);
        self.write_register(Self::register(&access), value, access.now);
        self.sync(drive);
        Ok(())
    }

    fn wait_states(&mut self, access: PeripheralAccess) -> Option<u8> {
        // no DTACK for an acknowledge with no interrupt pending (2.11)
        (access.role != Role::Iack || self.interrupting()).then_some(0)
    }

    fn interrupt_request(&self) -> bool {
        self.interrupting()
    }

    fn power_on(&mut self, drive: &mut Drive<'_>) {
        self.sync(drive);
    }

    fn reset(&mut self, now: Time, drive: &mut Drive<'_>) {
        self.run_until(now);
        self.hardware_reset(now);
        self.sync(drive);
    }

    fn input(&mut self, port: usize, signal: Signal, now: Time, drive: &mut Drive<'_>) {
        match (port, signal) {
            (RXDA | RXDB, Signal::Serial(serial)) => {
                // on the line first: a frame that starts as a break ends
                // continues it
                self.state.channels[usize::from(port == RXDB)].receive(serial, now);
                self.run_until(now);
            }
            (IP0..OP0, Signal::Level(high)) => {
                self.run_until(now);
                self.set_input(port - IP0, high, now);
            }
            _ => {}
        }
        self.sync(drive);
    }

    fn next_event(&self) -> Option<Time> {
        let source = self.source();
        let opcr = self.state.opcr;
        let every = opcr >> OPCR_OP3_SHIFT & OPCR_FIELD_MASK == OP3_COUNTER;
        let wanted = self.state.imr & ISR_COUNTER_READY != 0;
        let counter = self
            .state
            .counter
            .next_event(source, every, wanted)
            .map(|tick| self.crystal.time(tick));
        [self.next_internal(), counter].into_iter().flatten().min()
    }

    fn advance(&mut self, now: Time, drive: &mut Drive<'_>) {
        self.run_until(now);
        self.sync(drive);
    }

    /// Its registers, each channel's, the counter/timer and the ports,
    /// as of its last call: it catches up to board time only when the
    /// board calls it.
    fn inspect(&self) -> Inspection {
        let state = &self.state;
        let [a, b] = &state.channels;
        let outputs = self.output_levels().iter().enumerate().fold(
            Inspection::default(),
            |shown, (pin, level)| match level {
                Some(level) => shown.boolean(format!("OP{pin}"), *level),
                None => shown.text(format!("OP{pin}"), "clock"),
            },
        );
        let counter = &state.counter;
        Inspection::default()
            .text(
                "variant",
                match self.variant {
                    Variant::Mc68681 => MC68681,
                    Variant::Scc68692 => SCC68692,
                },
            )
            .group("a", a.inspect())
            .group("b", b.inspect())
            .register("acr", state.acr)
            .register("isr", self.interrupt_status())
            .register("imr", state.imr)
            .register("ivr", state.ivr)
            .register("opcr", state.opcr)
            .register("opr", state.opr)
            .boolean("irq", self.interrupting())
            .integer_in("input_port", self.input_port(), Radix::Binary)
            .group("outputs", outputs)
            .group(
                "counter",
                Inspection::default()
                    .register("preload", counter.preload)
                    .boolean("running", counter.running)
                    .boolean("ready", counter.ready)
                    .boolean("output", counter.output),
            )
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(
            VERSION,
            &(self.variant, self.crystal.hz, &self.state),
        ))
    }

    /// Refuses the other peripheral's snapshot, one of another X1/CLK, and one
    /// `State::check` refuses.
    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        let (variant, hz, state): (Variant, u64, State) = decode(VERSION, snapshot)?;
        if variant != self.variant {
            return Err(StateError::mismatch("the other DUART"));
        }
        if hz != self.crystal.hz {
            return Err(StateError::mismatch("a DUART of another X1/CLK"));
        }
        state.check(variant)?;
        self.state = state;
        self.transmitted.clear();
        Ok(())
    }
}
