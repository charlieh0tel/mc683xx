//! SingleStepTests' `m68000` binary format (`.json.bin`, as its
//! `decode.py` reads it): little-endian blocks, each a byte count and a
//! magic number, then its fields.  Bus transactions are skipped; cycle
//! counts are the 68000's, not the CPU32's.

use cpu32_oracle::state::State;

/// The file's magic number.
const FILE_MAGIC: u32 = 0x1a3f_5d71;
/// A test case's.
const CASE_MAGIC: u32 = 0xabc1_2367;
/// Its name's.
const NAME_MAGIC: u32 = 0x89ab_cdef;
/// A machine state's.
const STATE_MAGIC: u32 = 0x0123_4567;
/// Bytes in a block's header: its byte count and magic number.
const HEADER_BYTES: usize = 8;
/// RAM in the tests is the 68000's 24-bit address space.
const ADDRESS_LIMIT: u32 = 1 << 24;

/// What can be wrong with a test file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// It ends inside a field.
    #[error("truncated at byte {0}")]
    Truncated(usize),
    /// A block's magic number is wrong.
    #[error("bad magic {found:#010x} at byte {offset}, expected {expected:#010x}")]
    Magic {
        /// Where.
        offset: usize,
        /// The block's magic.
        expected: u32,
        /// What is there.
        found: u32,
    },
    /// A case's name is not UTF-8.
    #[error("case name at byte {0} is not UTF-8")]
    Name(usize),
    /// A RAM word outside the 24-bit address space.
    #[error("RAM address {0:#x} beyond 24 bits")]
    Address(u32),
}

/// A machine state: the registers and the RAM the case defines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The registers.  PC is the 68000's next prefetch address: the
    /// instruction's own address plus 4.
    pub registers: State,
    /// RAM, as even addresses and the words there.  Every byte the
    /// instruction reads is here, its opcode and extension words too.
    pub ram: Vec<(u32, u16)>,
}

/// One test case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    /// Its name: a number, the disassembly and the opcode.
    pub name: String,
    /// The state before the instruction.
    pub initial: Snapshot,
    /// The state after it.
    pub expected: Snapshot,
}

/// A cursor over the file's bytes.
struct Reader<'a> {
    /// The file.
    bytes: &'a [u8],
    /// Where the next field starts.
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], DecodeError> {
        let field = self
            .bytes
            .get(self.at..self.at + count)
            .ok_or(DecodeError::Truncated(self.at))?;
        self.at += count;
        Ok(field)
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        let field = self.take(2)?;
        Ok(u16::from_le_bytes([field[0], field[1]]))
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        let field = self.take(4)?;
        Ok(u32::from_le_bytes([field[0], field[1], field[2], field[3]]))
    }

    /// A block's header: where the block ends.
    fn header(&mut self, expected: u32) -> Result<usize, DecodeError> {
        let start = self.at;
        let length = self.u32()? as usize;
        let found = self.u32()?;
        if found != expected {
            return Err(DecodeError::Magic {
                offset: start,
                expected,
                found,
            });
        }
        if length < HEADER_BYTES {
            return Err(DecodeError::Truncated(start));
        }
        Ok(start + length)
    }

    fn name(&mut self) -> Result<String, DecodeError> {
        self.header(NAME_MAGIC)?;
        let length = self.u32()? as usize;
        let start = self.at;
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError::Name(start))
    }

    /// A state: D0-D7, A0-A6, USP, SSP, SR, PC, two prefetch words
    /// (also in RAM, so unused), then the RAM words.
    fn snapshot(&mut self) -> Result<Snapshot, DecodeError> {
        self.header(STATE_MAGIC)?;
        let mut data = [0; 8];
        for register in &mut data {
            *register = self.u32()?;
        }
        let mut address = [0; 7];
        for register in &mut address {
            *register = self.u32()?;
        }
        let usp = self.u32()?;
        let ssp = self.u32()?;
        let sr = self.u32()? as u16;
        let pc = self.u32()?;
        let _prefetch = (self.u32()?, self.u32()?);
        let words = self.u32()?;
        let ram = (0..words)
            .map(|_| {
                let at = self.u32()?;
                let word = self.u16()?;
                if at >= ADDRESS_LIMIT {
                    return Err(DecodeError::Address(at));
                }
                Ok((at, word))
            })
            .collect::<Result<_, _>>()?;
        Ok(Snapshot {
            registers: State {
                data,
                address,
                usp,
                ssp,
                sr,
                pc,
            },
            ram,
        })
    }

    fn case(&mut self) -> Result<Case, DecodeError> {
        let end = self.header(CASE_MAGIC)?;
        let name = self.name()?;
        let initial = self.snapshot()?;
        let expected = self.snapshot()?;
        if end > self.bytes.len() {
            return Err(DecodeError::Truncated(self.bytes.len()));
        }
        // The bus transactions and cycle count follow.
        self.at = end;
        Ok(Case {
            name,
            initial,
            expected,
        })
    }
}

/// The cases in a `.json.bin` file.
pub fn decode(bytes: &[u8]) -> Result<Vec<Case>, DecodeError> {
    let mut reader = Reader { bytes, at: 0 };
    let magic = reader.u32()?;
    if magic != FILE_MAGIC {
        return Err(DecodeError::Magic {
            offset: 0,
            expected: FILE_MAGIC,
            found: magic,
        });
    }
    let count = reader.u32()?;
    (0..count).map(|_| reader.case()).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::CASE_MAGIC;
    use super::Case;
    use super::DecodeError;
    use super::FILE_MAGIC;
    use super::HEADER_BYTES;
    use super::NAME_MAGIC;
    use super::STATE_MAGIC;
    use super::Snapshot;
    use super::decode;
    use cpu32_oracle::state::State;

    /// A bus transaction list's magic number.
    const TRANSACTIONS_MAGIC: u32 = 0x4567_89ab;

    /// A block: its byte count, `magic`, then `body`.
    fn block(magic: u32, body: &[u8]) -> Vec<u8> {
        let length = (HEADER_BYTES + body.len()) as u32;
        [&length.to_le_bytes()[..], &magic.to_le_bytes(), body].concat()
    }

    fn snapshot(snapshot: &Snapshot) -> Vec<u8> {
        let registers = &snapshot.registers;
        let fields = registers
            .data
            .iter()
            .chain(&registers.address)
            .copied()
            .chain([
                registers.usp,
                registers.ssp,
                u32::from(registers.sr),
                registers.pc,
                0,
                0,
                snapshot.ram.len() as u32,
            ]);
        let mut body: Vec<u8> = fields.flat_map(u32::to_le_bytes).collect();
        for (at, word) in &snapshot.ram {
            body.extend(at.to_le_bytes());
            body.extend(word.to_le_bytes());
        }
        block(STATE_MAGIC, &body)
    }

    /// Encodes `cases` as a `.json.bin` file with no transactions.
    pub(crate) fn encode(cases: &[Case]) -> Vec<u8> {
        let mut file = [FILE_MAGIC, cases.len() as u32]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<u8>>();
        for case in cases {
            let name = [
                &(case.name.len() as u32).to_le_bytes()[..],
                case.name.as_bytes(),
            ]
            .concat();
            let body = [
                block(NAME_MAGIC, &name),
                snapshot(&case.initial),
                snapshot(&case.expected),
                block(TRANSACTIONS_MAGIC, &[0; 8]),
            ]
            .concat();
            file.extend(block(CASE_MAGIC, &body));
        }
        file
    }

    #[test]
    fn decodes_what_encode_writes() {
        let state = State {
            data: [1, 2, 3, 4, 5, 6, 7, 8],
            sr: 0x2700,
            pc: 0x1004,
            ..State::default()
        };
        let case = Case {
            name: "000 NOP 4e71".to_owned(),
            initial: Snapshot {
                registers: state,
                ram: vec![(0x1000, 0x4e71)],
            },
            expected: Snapshot {
                registers: State {
                    pc: 0x1006,
                    ..state
                },
                ram: vec![(0x1000, 0x4e71)],
            },
        };
        let cases = vec![case.clone(), case];
        assert_eq!(decode(&encode(&cases)), Ok(cases));
    }

    #[test]
    fn rejects_other_files() {
        assert!(matches!(
            decode(&[0; 8]),
            Err(DecodeError::Magic { offset: 0, .. })
        ));
        assert_eq!(
            decode(&FILE_MAGIC.to_le_bytes()),
            Err(DecodeError::Truncated(4))
        );
    }
}
