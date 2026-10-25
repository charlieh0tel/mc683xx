//! Port QS (QSMRM 3.3; MC68331UM 6.2.2, D.4.8, D.4.9): the QSM's
//! eight pins as general-purpose I/O, or given to the QSPI by PQSPAR
//! and to the SCI by TE.

use crate::pins::PortLevels;

/// PQS0, MISO (MC68331UM Table D-14).
const MISO_PIN: u8 = 1;
/// PQS1, MOSI.
const MOSI_PIN: u8 = 1 << 1;
/// PQS2, SCK.
const SCK_PIN: u8 = 1 << 2;
/// PQS3, PCS0/SS: the first of the four PCS pins, PQS3 to PQS6.
pub(super) const PCS0_PIN: u32 = 3;
/// PCS\[3:0\]'s pins in the port.
pub(super) const PCS_PINS: u8 = 0b0111_1000;
/// The QSPI's pins, PQS0 to PQS6, which WOMQ makes open drain (QSMRM
/// 4.3.1).
const QSPI_PINS: u8 = 0b0111_1111;
/// PQS7, TXD.
const TXD_PIN: u8 = 1 << 7;
/// PQSPAR's implemented bits: PQSPA6 to PQSPA3, PQSPA1 and PQSPA0
/// (MC68331UM D.4.9; SCK and TXD have no assignment bit).
const PQSPAR_IMPLEMENTED: u8 = 0b0111_1011;

/// What the SCI makes of TXD (QSMRM 5.2.2, 5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Txd {
    /// Whether the transmitter has the pin, holding it at mark.
    pub(super) transmitter: bool,
    /// SCCR1's WOMS: as an output, TXD drives only low.
    pub(super) open_drain: bool,
}

/// Port QS: its data latch, data direction and pin assignment
/// registers, and the levels the board drives on its input pins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct PortQs {
    /// PORTQS's latch.
    pub(super) latch: u8,
    /// DDRQS: a set bit makes the pin an output.
    pub(super) direction: u8,
    /// PQSPAR: a set bit gives the pin to the QSPI.
    pub(super) assignment: u8,
    /// The levels the board drives on the pins.
    pub(super) pins: u8,
    /// SPCR0's WOMQ: the outputs among the QSPI's pins drive only low
    /// (QSMRM 4.3.1).
    pub(super) wired_or: bool,
}

impl PortQs {
    /// The port out of reset: every pin a general-purpose input
    /// (MC68331UM D.4.8, D.4.9); the board's pins start high.
    pub(super) const fn new() -> Self {
        Self {
            latch: 0,
            direction: 0,
            assignment: 0,
            pins: u8::MAX,
            wired_or: false,
        }
    }

    /// Whether `pin` is the QSPI's, by PQSPAR, and an input.
    const fn qspi_input(self, pin: u8) -> bool {
        self.assignment & pin != 0 && self.direction & pin == 0
    }

    /// Whether a slave takes data from MOSI: the QSPI's, an input
    /// (MC68331UM Table 6-1).
    pub(super) const fn mosi_input(self) -> bool {
        self.qspi_input(MOSI_PIN)
    }

    /// Whether a slave takes SCK: an input.  SCK has no PQSPAR bit; it
    /// is the QSPI's while SPE is set (MC68331UM Table 6-1, note 1).
    pub(super) const fn sck_input(self) -> bool {
        self.direction & SCK_PIN == 0
    }

    /// What a slave drives on MISO for a bit at `level`: nothing unless
    /// MISO is the QSPI's and an output (MC68331UM Table 6-1), and
    /// nothing for a high bit with WOMQ (QSMRM 4.3.1).
    pub(super) const fn miso(self, level: bool) -> Option<bool> {
        let output = self.assignment & MISO_PIN != 0 && self.direction & MISO_PIN != 0;
        if output && !(self.wired_or && level) {
            Some(level)
        } else {
            None
        }
    }

    /// Whether a snapshot's PQSPAR holds implemented bits only.
    pub(super) const fn is_valid(self) -> bool {
        self.assignment & !PQSPAR_IMPLEMENTED == 0
    }

    /// PQSPAR written.
    pub(super) const fn write_assignment(&mut self, value: u8) {
        self.assignment = value & PQSPAR_IMPLEMENTED;
    }

    /// Whether PCS0/SS is the QSPI's input: mode fault checking (QSMRM
    /// 4.3.5, MODF).
    pub(super) const fn slave_select_input(self) -> bool {
        self.qspi_input(1 << PCS0_PIN)
    }

    /// Whether the board holds PCS0/SS low.
    pub(super) const fn slave_select_low(self) -> bool {
        self.pins & 1 << PCS0_PIN == 0
    }

    /// The pins the chip drives and their levels, with the QSPI driving
    /// `code` on the PCS pins it has (QSMRM 4.3.6.3) and the SCI
    /// holding TXD at mark if its transmitter has it (5.3).  The QSPI's
    /// SCK, MOSI and MISO are not modeled at the bit level: they show
    /// the latch.  With WOMQ a QSPI pin at a high level floats (4.3.1),
    /// and with WOMS TXD (5.2.2).
    pub(super) const fn outputs(self, code: Option<u8>, txd: Txd) -> PortLevels {
        let (qspi, code) = match code {
            Some(code) => (
                self.assignment & self.direction & PCS_PINS,
                code << PCS0_PIN,
            ),
            None => (0, 0),
        };
        let (sci, mark) = if txd.transmitter {
            (TXD_PIN, TXD_PIN)
        } else {
            (0, 0)
        };
        let driven = self.direction | sci;
        let latched = self.direction & !qspi & !sci;
        let levels = self.latch & latched | code & qspi | mark;
        let open_drain =
            if self.wired_or { QSPI_PINS } else { 0 } | if txd.open_drain { TXD_PIN } else { 0 };
        let floating = levels & open_drain;
        PortLevels {
            driven: driven & !floating,
            levels: levels & !floating,
        }
    }

    /// The port's pins as PORTQS reads them: the driven level on
    /// outputs, the board's on inputs (QSMRM 3.3.1).
    pub(super) const fn levels(self, code: Option<u8>, txd: Txd) -> u8 {
        self.outputs(code, txd).pin_levels(self.pins)
    }

    /// The levels on PCS\[3:0\].
    pub(super) const fn chip_selects(self, code: Option<u8>) -> u8 {
        let txd = Txd {
            transmitter: false,
            open_drain: false,
        };
        (self.levels(code, txd) & PCS_PINS) >> PCS0_PIN
    }
}

#[cfg(test)]
mod tests {
    use crate::pins::PortLevels;

    use super::PortQs;
    use super::Txd;

    /// TXD the port's, push-pull.
    const PORT_TXD: Txd = Txd {
        transmitter: false,
        open_drain: false,
    };

    #[test]
    fn the_qspi_drives_only_its_assigned_outputs() {
        let mut port = PortQs::new();
        port.latch = 0x78;
        port.direction = 0x58;
        port.write_assignment(0xef);
        assert_eq!(port.assignment, 0x6b, "no SCK or TXD bit");
        port.pins = 0x00;
        // PCS0 and PCS3 the QSPI's code 0, PCS1 the latch, PCS2 an
        // input the board holds low
        assert_eq!(port.chip_selects(Some(0)), 0b0010);
        assert_eq!(port.chip_selects(None), 0b1011);
        let sci = Txd {
            transmitter: true,
            ..PORT_TXD
        };
        assert_eq!(port.levels(None, sci), 0xd8);
    }

    #[test]
    fn womq_floats_the_qspi_pins_driven_high() {
        let mut port = PortQs::new();
        port.latch = 0xa5;
        port.direction = 0xff;
        port.wired_or = true;
        port.pins = 0x00;
        // TXD keeps its push-pull driver
        assert_eq!(
            port.outputs(None, PORT_TXD),
            PortLevels {
                driven: 0xda,
                levels: 0x80
            }
        );
        assert_eq!(port.levels(None, PORT_TXD), 0x80);
        port.write_assignment(0x01);
        assert_eq!(port.miso(true), None);
        assert_eq!(port.miso(false), Some(false));
    }

    #[test]
    fn woms_floats_txd_driven_high() {
        let mut port = PortQs::new();
        port.direction = 0x80;
        port.pins = 0x00;
        let open_drain = Txd {
            open_drain: true,
            ..PORT_TXD
        };
        // PORTQS high: released, to the board's level (QSMRM 5.2.2)
        port.latch = 0x80;
        assert_eq!(
            port.outputs(None, open_drain),
            PortLevels {
                driven: 0x00,
                levels: 0x00
            }
        );
        assert_eq!(port.levels(None, open_drain), 0x00);
        port.pins = 0x80;
        assert_eq!(port.levels(None, open_drain), 0x80);
        // low: driven
        port.latch = 0x00;
        port.pins = 0x80;
        assert_eq!(
            port.outputs(None, open_drain),
            PortLevels {
                driven: 0x80,
                levels: 0x00
            }
        );
        // the transmitter's mark floats too, and push-pull drives it
        port.direction = 0x00;
        let transmitting = Txd {
            transmitter: true,
            open_drain: true,
        };
        port.pins = 0x00;
        assert_eq!(
            port.outputs(None, transmitting),
            PortLevels {
                driven: 0x00,
                levels: 0x00
            }
        );
        let push_pull = Txd {
            open_drain: false,
            ..transmitting
        };
        assert_eq!(
            port.outputs(None, push_pull),
            PortLevels {
                driven: 0x80,
                levels: 0x80
            }
        );
    }
}
