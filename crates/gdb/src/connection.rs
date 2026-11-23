//! gdb's connection: what [`serve`](crate::serve) drives
//! ([`Connection`]), a TCP listener for one gdb, and a connection whose
//! waits keep a host responsive ([`Polled`]).

use std::io;
use std::io::Read;
use std::io::Write;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::TcpStream;
use std::net::ToSocketAddrs;
use std::thread;
use std::time::Duration;

/// gdb's end of a connection, as [`serve`](crate::serve) drives it: a
/// [`TcpStream`], a [`Polled`] one, or a host's own, an in-memory one
/// for a test among them.
pub trait Connection {
    /// Why the connection failed, gdb gone among them.
    type Error: std::error::Error;

    /// Writes all of `bytes`.
    ///
    /// # Errors
    ///
    /// The connection's, which ends the session.
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;

    /// Sends what was written.
    ///
    /// # Errors
    ///
    /// The connection's, which ends the session.
    fn flush(&mut self) -> Result<(), Self::Error>;

    /// The next byte, waiting for it.
    ///
    /// # Errors
    ///
    /// The connection's, gdb gone among them, which ends the session.
    fn read(&mut self) -> Result<u8, Self::Error>;

    /// The next byte if one has come, without waiting for it, left for
    /// [`read`](Self::read): the stub looks between a run's quanta for
    /// gdb's Ctrl-C.
    ///
    /// # Errors
    ///
    /// The connection's, gdb gone among them, which ends the session.
    fn peek(&mut self) -> Result<Option<u8>, Self::Error>;

    /// Called once, as the session starts; by default nothing.
    ///
    /// # Errors
    ///
    /// The connection's, which ends the session.
    fn start(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// A connection borrowed for a session: `serve(target, &mut wire)`.
impl<C: Connection + ?Sized> Connection for &mut C {
    type Error = C::Error;

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), C::Error> {
        (**self).write_all(bytes)
    }

    fn flush(&mut self) -> Result<(), C::Error> {
        (**self).flush()
    }

    fn read(&mut self) -> Result<u8, C::Error> {
        (**self).read()
    }

    fn peek(&mut self) -> Result<Option<u8>, C::Error> {
        (**self).peek()
    }

    fn start(&mut self) -> Result<(), C::Error> {
        (**self).start()
    }
}

/// Blocking but for [`peek`](Connection::peek), and without Nagle's
/// delay once the session starts: gdb's packets are small, and each
/// waits for its answer.
impl Connection for TcpStream {
    type Error = io::Error;

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        Write::write_all(self, bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(self)
    }

    fn read(&mut self) -> io::Result<u8> {
        let mut byte = [0];
        Read::read_exact(self, &mut byte)?;
        Ok(byte[0])
    }

    fn peek(&mut self) -> io::Result<Option<u8>> {
        peek_stream(self)
    }

    fn start(&mut self) -> io::Result<()> {
        self.set_nodelay(true)
    }
}

/// The next byte on `stream` if one has come, without waiting for it
/// or taking it; gdb having closed the connection, an
/// [`UnexpectedEof`](io::ErrorKind::UnexpectedEof) error.
fn peek_stream(stream: &TcpStream) -> io::Result<Option<u8>> {
    stream.set_nonblocking(true)?;
    let mut byte = [0];
    let peeked = stream.peek(&mut byte);
    stream.set_nonblocking(false)?;
    match peeked {
        Ok(0) => Err(io::ErrorKind::UnexpectedEof.into()),
        Ok(_) => Ok(Some(byte[0])),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error),
    }
}

/// The longest a [`Polled`] connection waits for gdb before it calls
/// its host's [`Waiter::idle`].
pub const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A host that must stay responsive while the stub waits for gdb.
///
/// A Python host lets other threads run during each wait and runs its
/// signal handlers between them.
pub trait Waiter {
    /// What ends the session from the host's side.
    type Error: std::error::Error;

    /// Runs `wait`, which blocks for at most [`POLL_INTERVAL`].
    fn wait<R: Send>(&mut self, wait: impl FnOnce() -> R + Send) -> R;

    /// Called after each wait in which gdb sent nothing.
    ///
    /// # Errors
    ///
    /// The host's reason to end the session, which ends it as a
    /// [`PollError::Host`].
    fn idle(&mut self) -> Result<(), Self::Error>;
}

/// A [`Polled`] connection's error: the connection's, or the host's.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PollError<E> {
    /// The connection failed, or gdb closed it.
    #[error(transparent)]
    Connection(#[from] io::Error),
    /// The host's [`idle`](Waiter::idle) or `ready` ended the session,
    /// with its error, which this shows as its own: its message and
    /// its `source()`.
    #[error(transparent)]
    Host(E),
}

/// A TCP connection to gdb whose waits for bytes last up to
/// [`POLL_INTERVAL`] each, the host's [`Waiter`] around and between
/// them; for [`serve`](crate::serve).
#[derive(Debug)]
pub struct Polled<W> {
    /// The connection.
    stream: TcpStream,
    /// The host.
    waiter: W,
}

/// [`accept`], waiting for gdb in waits of [`POLL_INTERVAL`] through
/// `waiter`, and the connection a [`Polled`] one.
///
/// # Errors
///
/// [`PollError::Connection`] if the listener cannot be bound or a
/// connection accepted or set up; [`PollError::Host`] if `ready` or
/// the waiter's [`idle`](Waiter::idle) fails.
pub fn accept_polled<W: Waiter>(
    address: impl ToSocketAddrs,
    ready: impl FnOnce(SocketAddr) -> Result<(), W::Error>,
    mut waiter: W,
) -> Result<Polled<W>, PollError<W::Error>> {
    let listener = TcpListener::bind(address)?;
    ready(listener.local_addr()?).map_err(PollError::Host)?;
    listener.set_nonblocking(true)?;
    let stream = loop {
        let accepted = waiter.wait(|| {
            let accepted = listener.accept();
            if accepted.as_ref().is_err_and(waiting) {
                thread::sleep(POLL_INTERVAL);
            }
            accepted
        });
        match accepted {
            Ok((stream, _)) => break stream,
            Err(error) if waiting(&error) => waiter.idle().map_err(PollError::Host)?,
            Err(error) => return Err(error.into()),
        }
    };
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(POLL_INTERVAL))?;
    Ok(Polled { stream, waiter })
}

/// Whether `error` is a wait that ended with nothing: a timeout, or a
/// signal.
fn waiting(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

impl<W: Waiter> Connection for Polled<W> {
    type Error = PollError<W::Error>;

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        Ok(Write::write_all(&mut self.stream, bytes)?)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(Write::flush(&mut self.stream)?)
    }

    fn read(&mut self) -> Result<u8, Self::Error> {
        let stream = &self.stream;
        loop {
            let read = self.waiter.wait(|| {
                let mut byte = [0];
                (&*stream).read(&mut byte).map(|count| (count, byte[0]))
            });
            match read {
                Ok((0, _)) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into()),
                Ok((_, byte)) => return Ok(byte),
                Err(error) if waiting(&error) => self.waiter.idle().map_err(PollError::Host)?,
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Between a run's quanta: the host's wait around it lets other
    /// threads in (gdb's own client among them).
    fn peek(&mut self) -> Result<Option<u8>, Self::Error> {
        let stream = &self.stream;
        Ok(self.waiter.wait(|| peek_stream(stream))?)
    }

    fn start(&mut self) -> Result<(), Self::Error> {
        Ok(self.stream.set_nodelay(true)?)
    }
}

/// Listens at `address` for one gdb, tells `ready` the address it
/// listens at (a port of 0 picks one), and takes the first connection.
///
/// # Errors
///
/// `ready`'s error, or the I/O error, as `E`, if the listener cannot
/// be bound or a connection accepted.
pub fn accept<E: From<io::Error>>(
    address: impl ToSocketAddrs,
    ready: impl FnOnce(SocketAddr) -> Result<(), E>,
) -> Result<TcpStream, E> {
    let listener = TcpListener::bind(address)?;
    ready(listener.local_addr()?)?;
    let (stream, _) = listener.accept()?;
    Ok(stream)
}
