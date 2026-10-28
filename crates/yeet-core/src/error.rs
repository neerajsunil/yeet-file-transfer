//! The engine's error type. Variants are what a UI needs to tell people what happened;
//! anything else is an [`Error::Io`] with the OS error inside.

use std::{fmt, io};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// This side cancelled.
    Cancelled,
    /// The other device cancelled.
    CancelledByPeer,
    /// The receiver declined the offer.
    Declined,
    /// Nobody answered the offer in time.
    NoAnswer,
    /// The connection went quiet: no data or heartbeat within the timeout.
    LostContact,
    /// The sender disconnected and didn't come back within the resume window.
    PeerDidNotReturn,
    /// No address for the device, or none of them answered.
    Unreachable,
    /// The other side sent something that doesn't follow the protocol (or is a different version).
    Protocol(String),
    /// The sender refused something unsafe, e.g. a path that would escape the destination.
    Unsafe(String),
    Io(io::Error),
}

impl Error {
    pub(crate) fn protocol(msg: impl Into<String>) -> Error {
        Error::Protocol(msg.into())
    }

    /// True for errors that mean "the network went away" rather than "this transfer can't work",
    /// i.e. the ones worth reconnecting and resuming after.
    pub fn is_disconnect(&self) -> bool {
        use io::ErrorKind::*;
        match self {
            Error::LostContact | Error::Unreachable => true,
            Error::Io(e) => matches!(
                e.kind(),
                ConnectionReset
                    | ConnectionAborted
                    | ConnectionRefused
                    | BrokenPipe
                    | TimedOut
                    | WouldBlock
                    | UnexpectedEof
                    | NotConnected
                    | NetworkUnreachable
                    | HostUnreachable
                    | NetworkDown
                    | AddrNotAvailable
            ),
            _ => false,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Cancelled => f.write_str("cancelled"),
            Error::CancelledByPeer => f.write_str("cancelled on the other device"),
            Error::Declined => f.write_str("declined"),
            Error::NoAnswer => f.write_str("no answer from the other device"),
            Error::LostContact => f.write_str("lost contact with the other device"),
            Error::PeerDidNotReturn => f.write_str("the other device didn't come back"),
            Error::Unreachable => f.write_str("device not reachable"),
            Error::Protocol(m) => write!(f, "protocol error: {m}"),
            Error::Unsafe(m) => write!(f, "refused: {m}"),
            Error::Io(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        // Socket read timeouts surface as these; give them a meaning.
        match e.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => Error::LostContact,
            _ => Error::Io(e),
        }
    }
}

impl From<walkdir::Error> for Error {
    fn from(e: walkdir::Error) -> Error {
        Error::Io(e.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeouts_become_lost_contact() {
        let e: Error = io::Error::from(io::ErrorKind::TimedOut).into();
        assert!(matches!(e, Error::LostContact));
        assert!(e.is_disconnect());
    }

    #[test]
    fn decisions_are_not_disconnects() {
        for e in [Error::Declined, Error::Cancelled, Error::CancelledByPeer, Error::protocol("x")] {
            assert!(!e.is_disconnect(), "{e}");
        }
        assert!(Error::Io(io::ErrorKind::ConnectionReset.into()).is_disconnect());
        assert!(!Error::Io(io::ErrorKind::PermissionDenied.into()).is_disconnect());
    }
}
