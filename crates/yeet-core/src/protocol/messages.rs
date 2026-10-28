//! Frames with structured payloads.

use super::cursor::{put_str16, Cursor};
use super::{frame, Codec, Kind};
use crate::{Error, Result};

/// What the sender wants to send, so the receiver can decide before anything is written.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    /// Identifies this transfer across reconnects.
    pub transfer: u64,
    /// The sender's display name.
    pub name: String,
    pub files: u64,
    pub bytes: u64,
    /// Top-level file and folder names.
    pub items: Vec<String>,
}

impl Offer {
    pub fn encode(&self) -> Vec<u8> {
        let items = &self.items[..self.items.len().min(u16::MAX as usize)];
        frame(Kind::Offer, 64, |v| {
            v.extend_from_slice(&self.transfer.to_le_bytes());
            v.extend_from_slice(&self.files.to_le_bytes());
            v.extend_from_slice(&self.bytes.to_le_bytes());
            put_str16(v, &self.name);
            v.extend_from_slice(&(items.len() as u16).to_le_bytes());
            for it in items {
                put_str16(v, it);
            }
        })
    }

    pub fn decode(payload: &[u8]) -> Result<Offer> {
        let mut c = Cursor::new(payload);
        let transfer = c.u64()?;
        let (files, bytes) = (c.u64()?, c.u64()?);
        let name = c.str16()?;
        let items = (0..c.u16()?).map(|_| c.str16()).collect::<Result<_>>()?;
        Ok(Offer { transfer, name, files, bytes, items })
    }
}

/// File and byte counts. Used by ACK (progress so far), DONE (what was sent) and SUMMARY
/// (what the receiver ended up with).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Totals {
    pub files: u64,
    pub bytes: u64,
}

impl Totals {
    pub fn encode(&self, kind: Kind) -> Vec<u8> {
        frame(kind, 16, |v| {
            v.extend_from_slice(&self.files.to_le_bytes());
            v.extend_from_slice(&self.bytes.to_le_bytes());
        })
    }

    pub fn decode(payload: &[u8]) -> Result<Totals> {
        let mut c = Cursor::new(payload);
        Ok(Totals { files: c.u64()?, bytes: c.u64()? })
    }
}

/// A slice of one file inside a chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seg {
    pub id: u64,
    pub offset: u64,
    pub len: u32,
}

/// Up to ~4 MiB of raw data made of one or more segments. Small files are bundled many per
/// chunk; large files are split across chunks. Segments carry their file and offset, so chunks
/// can be written in any order.
#[derive(Debug)]
pub struct Chunk<'a> {
    pub codec: Codec,
    /// Size of the data once decoded; always the sum of the segment lengths.
    pub raw_len: u32,
    pub segs: Vec<Seg>,
    pub data: &'a [u8],
}

impl Chunk<'_> {
    pub fn encode(&self) -> Vec<u8> {
        frame(Kind::Chunk, 7 + self.segs.len() * 20 + self.data.len(), |v| {
            v.push(self.codec as u8);
            v.extend_from_slice(&self.raw_len.to_le_bytes());
            v.extend_from_slice(&(self.segs.len() as u16).to_le_bytes());
            for s in &self.segs {
                v.extend_from_slice(&s.id.to_le_bytes());
                v.extend_from_slice(&s.offset.to_le_bytes());
                v.extend_from_slice(&s.len.to_le_bytes());
            }
            v.extend_from_slice(self.data);
        })
    }

    pub fn decode(payload: &[u8]) -> Result<Chunk<'_>> {
        let mut c = Cursor::new(payload);
        let codec = Codec::try_from(c.u8()?)?;
        let raw_len = c.u32()?;
        let n = c.u16()? as usize;
        let segs =
            (0..n).map(|_| Ok(Seg { id: c.u64()?, offset: c.u64()?, len: c.u32()? })).collect::<Result<Vec<_>>>()?;
        if segs.iter().map(|s| s.len as u64).sum::<u64>() != raw_len as u64 {
            return Err(Error::protocol("segment lengths don't match chunk size"));
        }
        Ok(Chunk { codec, raw_len, segs, data: c.rest() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::read_frame;

    fn payload(frame: Vec<u8>, kind: Kind) -> Vec<u8> {
        let (k, p) = read_frame(&mut frame.as_slice()).unwrap();
        assert_eq!(k, kind);
        p
    }

    #[test]
    fn offer_roundtrip() {
        let o =
            Offer { transfer: 42, name: "Laptop".into(), files: 3, bytes: 99, items: vec!["a".into(), "b c".into()] };
        assert_eq!(Offer::decode(&payload(o.encode(), Kind::Offer)).unwrap(), o);
    }

    #[test]
    fn totals_roundtrip() {
        let t = Totals { files: 66_077, bytes: 1_394_654_682 };
        assert_eq!(Totals::decode(&payload(t.encode(Kind::Ack), Kind::Ack)).unwrap(), t);
    }

    #[test]
    fn chunk_roundtrip() {
        let segs = vec![Seg { id: 1, offset: 0, len: 3 }, Seg { id: 2, offset: 10, len: 2 }];
        let c = Chunk { codec: Codec::None, raw_len: 5, segs: segs.clone(), data: b"abcde" };
        let p = payload(c.encode(), Kind::Chunk);
        let d = Chunk::decode(&p).unwrap();
        assert_eq!((d.codec, d.raw_len, d.segs, d.data), (Codec::None, 5, segs, &b"abcde"[..]));
    }

    #[test]
    fn chunk_rejects_inconsistent_sizes() {
        let c = Chunk { codec: Codec::None, raw_len: 9, segs: vec![Seg { id: 1, offset: 0, len: 3 }], data: b"abc" };
        assert!(Chunk::decode(&payload(c.encode(), Kind::Chunk)).is_err());
    }
}
