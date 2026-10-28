use crate::{Error, Result};

/// Reads little-endian values off the front of a payload, failing cleanly when it's too short.
pub struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    pub fn new(bytes: &'a [u8]) -> Cursor<'a> {
        Cursor(bytes)
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(Error::protocol("truncated frame"));
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().expect("take returns exactly N bytes"))
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        self.array().map(u16::from_le_bytes)
    }

    pub fn u32(&mut self) -> Result<u32> {
        self.array().map(u32::from_le_bytes)
    }

    pub fn u64(&mut self) -> Result<u64> {
        self.array().map(u64::from_le_bytes)
    }

    /// A string prefixed with its u16 byte length.
    pub fn str16(&mut self) -> Result<String> {
        let n = self.u16()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| Error::protocol("string is not UTF-8"))
    }

    /// Everything left, as UTF-8.
    pub fn rest_str(self) -> Result<&'a str> {
        std::str::from_utf8(self.0).map_err(|_| Error::protocol("path is not UTF-8"))
    }

    /// Everything left, as bytes.
    pub fn rest(self) -> &'a [u8] {
        self.0
    }
}

/// Appends a string prefixed with its u16 byte length, truncating at 64 KiB.
pub(crate) fn put_str16(v: &mut Vec<u8>, s: &str) {
    let mut end = s.len().min(u16::MAX as usize);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    v.extend_from_slice(&(end as u16).to_le_bytes());
    v.extend_from_slice(&s.as_bytes()[..end]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_in_order_and_stops_at_the_end() {
        let mut v = vec![1u8];
        v.extend_from_slice(&2u16.to_le_bytes());
        v.extend_from_slice(&3u64.to_le_bytes());
        put_str16(&mut v, "héllo");
        let mut c = Cursor::new(&v);
        assert_eq!(c.u8().unwrap(), 1);
        assert_eq!(c.u16().unwrap(), 2);
        assert_eq!(c.u64().unwrap(), 3);
        assert_eq!(c.str16().unwrap(), "héllo");
        assert!(c.u8().is_err());
    }

    #[test]
    fn long_strings_truncate_on_a_char_boundary() {
        let s = "é".repeat(40_000); // 80,000 bytes
        let mut v = Vec::new();
        put_str16(&mut v, &s);
        let out = Cursor::new(&v).str16().unwrap();
        assert!(out.len() <= u16::MAX as usize && s.starts_with(&out));
    }
}
