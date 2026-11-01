//! gdb's end of an in-memory connection, for the stubs' tests: raw
//! remote serial protocol packets, each sent once the stub has answered
//! the last, as gdb waits, and the stub's replies decoded.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::iter;
use std::rc::Rc;

use gdbstub::conn::Connection;
use gdbstub::conn::ConnectionExt;

/// What the test sends: a packet once the last is answered, or bytes
/// at once.
pub(crate) enum Send {
    /// A packet's body.
    Packet(String),
    /// Raw bytes, sent as soon as the stub reads.
    Now(Vec<u8>),
}

/// A packet's body, sent once the last is answered.
pub(crate) fn packet(body: &str) -> Send {
    Send::Packet(body.to_owned())
}

/// The connection: the script in, what the stub wrote out.
pub(crate) struct Wire {
    /// What is left to send.
    script: VecDeque<Send>,
    /// The bytes sent, not yet read.
    input: VecDeque<u8>,
    /// What the stub wrote.
    output: Rc<RefCell<Vec<u8>>>,
    /// Packets the stub had begun when the last packet went.
    answered: Option<usize>,
}

impl Wire {
    /// Sends the next of the script if the stub may read it.
    fn refill(&mut self) {
        if !self.input.is_empty() {
            return;
        }
        let begun = self.output.borrow().iter().filter(|&&b| b == b'$').count();
        let ready = match self.script.front() {
            Some(Send::Now(_)) => true,
            Some(Send::Packet(_)) => self.answered.is_none_or(|before| begun > before),
            None => false,
        };
        if !ready {
            return;
        }
        match self.script.pop_front() {
            Some(Send::Now(bytes)) => self.input.extend(bytes),
            Some(Send::Packet(body)) => {
                let sum = body.bytes().fold(0u8, u8::wrapping_add);
                self.input.extend(format!("${body}#{sum:02x}").bytes());
                self.answered = Some(begun);
            }
            None => {}
        }
    }
}

impl Connection for Wire {
    type Error = io::Error;

    fn write(&mut self, byte: u8) -> io::Result<()> {
        self.output.borrow_mut().push(byte);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl ConnectionExt for Wire {
    fn read(&mut self) -> io::Result<u8> {
        self.refill();
        self.input
            .pop_front()
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))
    }

    /// The next byte, if one has come; end of file once the script is
    /// spent, as gdb gone.
    fn peek(&mut self) -> io::Result<Option<u8>> {
        self.refill();
        if self.input.is_empty() && self.script.is_empty() {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        Ok(self.input.front().copied())
    }
}

/// The packets the stub sent, their bodies run-length decoded.
pub(crate) fn replies(output: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(output);
    text.split('$')
        .skip(1)
        .filter_map(|packet| packet.split_once('#'))
        .map(|(body, _)| {
            let mut decoded = String::new();
            let mut chars = body.chars();
            while let Some(c) = chars.next() {
                match c {
                    '*' => {
                        let count = chars.next().map_or(0, |n| n as usize - 29);
                        let last = decoded.chars().last().unwrap_or('0');
                        decoded.extend(iter::repeat_n(last, count));
                    }
                    other => decoded.push(other),
                }
            }
            decoded
        })
        .collect()
}

/// A `g` reply's registers.
pub(crate) fn registers(reply: &str) -> Vec<u32> {
    (0..reply.len() / 8)
        .map(|index| u32::from_str_radix(&reply[8 * index..8 * index + 8], 16).unwrap())
        .collect()
}

/// `words` as big-endian bytes.
pub(crate) fn bytes(words: &[u16]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_be_bytes()).collect()
}

/// `text` as hex, as `qRcmd` carries it.
pub(crate) fn hex(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02x}")).collect()
}

impl Wire {
    /// A connection that sends `script` and keeps what the stub writes
    /// in `output`.
    pub(crate) fn new(script: Vec<Send>, output: &Rc<RefCell<Vec<u8>>>) -> Self {
        Self {
            script: script.into(),
            input: VecDeque::new(),
            output: Rc::clone(output),
            answered: None,
        }
    }
}
