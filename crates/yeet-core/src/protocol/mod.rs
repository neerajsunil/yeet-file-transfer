//! Wire format. The full description, with byte layouts, is in `docs/PROTOCOL.md`.
//!
//! After a 4-byte magic, everything is a frame: `[kind u8][len u32 LE][payload]`.
//!
//! ```text
//! sender                                   receiver
//!   OFFER (transfer id, name, totals) ──▶
//!                                       ◀── ACCEPT + HAVE (bytes it already has), or DECLINE
//!   DIR / FILE / CHUNK ... DONE        ──▶
//!                                       ◀── ACK every 300 ms (progress, doubles as heartbeat)
//!                                       ◀── SUMMARY
//! ```
//!
//! Either side may send CANCEL to stop for good. A dropped connection is different: the sender
//! reconnects with the same transfer id, and the receiver's HAVE says where to resume.

mod cursor;
mod messages;

pub use cursor::Cursor;
pub use messages::{Chunk, Offer, Seg, Totals};

use crate::{Error, Result};
use std::io::Read;

/// Identifies the protocol and its version. Changes whenever the format does.
pub const MAGIC: &[u8; 4] = b"YET1";
/// Target raw size of a chunk.
pub const BLOCK: usize = 4 << 20;
/// Files below this size are bundled with other small files into shared chunks.
pub const SMALL: u64 = 1 << 20;
/// Upper bound on a frame's payload. HAVE can list many files; everything else is at most a chunk.
const MAX_FRAME: usize = 64 * BLOCK;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Dir = 1,
    File = 2,
    Chunk = 3,
    Done = 4,
    Summary = 5,
    Offer = 6,
    Accept = 7,
    Decline = 8,
    Have = 9,
    Ack = 10,
    Cancel = 11,
    Local = 12,
}

impl TryFrom<u8> for Kind {
    type Error = Error;
    fn try_from(b: u8) -> Result<Kind> {
        use Kind::*;
        Ok(match b {
            1 => Dir,
            2 => File,
            3 => Chunk,
            4 => Done,
            5 => Summary,
            6 => Offer,
            7 => Accept,
            8 => Decline,
            9 => Have,
            10 => Ack,
            11 => Cancel,
            12 => Local,
            _ => return Err(Error::protocol(format!("unknown frame kind {b}"))),
        })
    }
}

/// How a chunk's data is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Codec {
    None = 0,
    Zstd = 1,
}

impl TryFrom<u8> for Codec {
    type Error = Error;
    fn try_from(b: u8) -> Result<Codec> {
        match b {
            0 => Ok(Codec::None),
            1 => Ok(Codec::Zstd),
            _ => Err(Error::protocol(format!("unknown codec {b}"))),
        }
    }
}

/// Builds one frame: reserves the header, lets `body` append the payload, then fills in the length.
pub(crate) fn frame(kind: Kind, capacity: usize, body: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut v = Vec::with_capacity(5 + capacity);
    v.push(kind as u8);
    v.extend_from_slice(&[0; 4]);
    body(&mut v);
    let len = (v.len() - 5) as u32;
    v[1..5].copy_from_slice(&len.to_le_bytes());
    v
}

/// A frame with no payload (ACCEPT, DECLINE, CANCEL).
pub fn empty(kind: Kind) -> Vec<u8> {
    frame(kind, 0, |_| {})
}

/// Yes to an offer: what the destination drive is, so the sender can pace for it, and whether
/// the receiver will copy files itself from paths (only when the sender is this same computer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Accept {
    pub disk: crate::disk::Disk,
    pub local: bool,
}

impl Accept {
    pub fn encode(&self) -> Vec<u8> {
        frame(Kind::Accept, 3, |v| {
            v.extend_from_slice(&self.disk.encode());
            v.push(self.local as u8);
        })
    }

    pub fn decode(payload: &[u8]) -> Accept {
        Accept { disk: crate::disk::Disk::decode(payload), local: payload.get(2) == Some(&1) }
    }
}

/// Instead of CHUNKs, on the same computer: the receiver copies file `id` from `path` itself.
pub fn local(id: u64, path: &str) -> Vec<u8> {
    frame(Kind::Local, 8 + path.len(), |v| {
        v.extend_from_slice(&id.to_le_bytes());
        v.extend_from_slice(path.as_bytes());
    })
}

/// Parses a LOCAL payload into (id, source path).
pub fn parse_local(payload: &[u8]) -> Result<(u64, &str)> {
    let mut c = Cursor::new(payload);
    Ok((c.u64()?, c.rest_str()?))
}

/// A directory to create, as a `/`-separated path relative to the destination.
pub fn dir(path: &str) -> Vec<u8> {
    frame(Kind::Dir, path.len(), |v| v.extend_from_slice(path.as_bytes()))
}

/// A file that chunks will refer to by `id`.
pub fn file(id: u64, size: u64, path: &str) -> Vec<u8> {
    frame(Kind::File, 16 + path.len(), |v| {
        v.extend_from_slice(&id.to_le_bytes());
        v.extend_from_slice(&size.to_le_bytes());
        v.extend_from_slice(path.as_bytes());
    })
}

/// (file id, bytes already written) for every file the receiver knows about.
pub fn have(have: &[(u64, u64)]) -> Vec<u8> {
    frame(Kind::Have, 4 + have.len() * 16, |v| {
        v.extend_from_slice(&(have.len() as u32).to_le_bytes());
        for (id, n) in have {
            v.extend_from_slice(&id.to_le_bytes());
            v.extend_from_slice(&n.to_le_bytes());
        }
    })
}

pub fn parse_have(payload: &[u8]) -> Result<Vec<(u64, u64)>> {
    let mut c = Cursor::new(payload);
    (0..c.u32()?).map(|_| Ok((c.u64()?, c.u64()?))).collect()
}

/// Parses a FILE payload into (id, size, path).
pub fn parse_file(payload: &[u8]) -> Result<(u64, u64, &str)> {
    let mut c = Cursor::new(payload);
    Ok((c.u64()?, c.u64()?, c.rest_str()?))
}

/// Reads one frame. Socket timeouts come back as [`Error::LostContact`].
pub fn read_frame(r: &mut impl Read) -> Result<(Kind, Vec<u8>)> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let kind = Kind::try_from(head[0])?;
    let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_FRAME {
        return Err(Error::protocol("frame too large"));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok((kind, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(bytes: Vec<u8>) -> (Kind, Vec<u8>) {
        read_frame(&mut bytes.as_slice()).unwrap()
    }

    #[test]
    fn file_roundtrip() {
        let (kind, p) = roundtrip(file(7, 1234, "game/data/a.pak"));
        assert_eq!(kind, Kind::File);
        assert_eq!(parse_file(&p).unwrap(), (7, 1234, "game/data/a.pak"));
    }

    #[test]
    fn have_roundtrip() {
        let list = vec![(0, 10), (5, 0), (u64::MAX, u64::MAX)];
        let (kind, p) = roundtrip(have(&list));
        assert_eq!(kind, Kind::Have);
        assert_eq!(parse_have(&p).unwrap(), list);
    }

    #[test]
    fn accept_and_local_roundtrip() {
        use crate::disk::{Disk, Fs, Media};
        let a = Accept { disk: Disk { media: Media::Hdd, fs: Fs::Ntfs }, local: true };
        let (kind, p) = roundtrip(a.encode());
        assert_eq!((kind, Accept::decode(&p)), (Kind::Accept, a));
        assert!(!Accept::decode(&[]).local);
        let (kind, p) = roundtrip(local(3, r"C:\src\a.bin"));
        assert_eq!((kind, parse_local(&p).unwrap()), (Kind::Local, (3, r"C:\src\a.bin")));
    }

    #[test]
    fn empty_frames() {
        assert_eq!(roundtrip(empty(Kind::Cancel)), (Kind::Cancel, vec![]));
    }

    #[test]
    fn rejects_unknown_kind_and_huge_frames() {
        let mut bad = empty(Kind::Accept);
        bad[0] = 200;
        assert!(read_frame(&mut bad.as_slice()).is_err());
        let mut huge = empty(Kind::Chunk);
        huge[1..5].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(read_frame(&mut huge.as_slice()), Err(Error::Protocol(_))));
    }

    #[test]
    fn truncated_frame_is_an_error() {
        let f = file(1, 2, "abc");
        assert!(read_frame(&mut &f[..f.len() - 1]).is_err());
    }
}
