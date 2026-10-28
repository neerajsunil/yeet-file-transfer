//! Reads planned data from disk and turns it into CHUNK frames, compressing only when it pays.

use super::plan::Plan;
use super::Compress;
use crate::protocol::{Chunk, Codec, Seg, BLOCK, SMALL};
use crate::{Error, Result};
use crossbeam_channel::{Receiver, Sender};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Mutex;

/// After this many incompressible chunks in a row, stop trying for a file type...
const GIVE_UP_AFTER: u32 = 3;
/// ...but re-probe every this many chunks in case the data changes.
const REPROBE_EVERY: u32 = 16;
/// Compression must save at least this fraction to be used.
const MIN_SAVING: f64 = 0.05;

/// Per file type, whether compression has been paying off. Shared by all packers, so one
/// packer's discovery that `.mp4` doesn't shrink saves the others the effort.
#[derive(Default)]
pub(super) struct ClassMemory(Mutex<HashMap<String, Record>>);

#[derive(Default)]
struct Record {
    /// Chunks in a row that didn't shrink enough.
    fails: u32,
    /// Chunks skipped since giving up.
    skipped: u32,
}

impl ClassMemory {
    pub fn should_try(&self, class: &str) -> bool {
        let mut m = self.0.lock().expect("class memory poisoned");
        let r = m.entry(class.to_string()).or_default();
        if r.fails < GIVE_UP_AFTER {
            return true;
        }
        r.skipped += 1;
        if r.skipped >= REPROBE_EVERY {
            r.skipped = 0;
            return true;
        }
        false
    }

    pub fn record(&self, class: &str, worth_it: bool) {
        let mut m = self.0.lock().expect("class memory poisoned");
        let r = m.entry(class.to_string()).or_default();
        r.fails = if worth_it { 0 } else { r.fails + 1 };
    }
}

pub(super) fn packer(
    plans: Receiver<Plan>,
    frames: Sender<Vec<u8>>,
    compress: Compress,
    memory: &ClassMemory,
) -> Result<()> {
    let mut buf = Vec::with_capacity(BLOCK + SMALL as usize);
    let mut zstd = match compress {
        Compress::Auto(level) | Compress::Zstd(level) => Some(zstd::bulk::Compressor::new(level)?),
        Compress::None => None,
    };
    for plan in plans {
        buf.clear();
        for (seg, path) in &plan.segs {
            read_segment(path, seg, &mut buf)?;
        }
        let packed = match zstd.as_mut() {
            Some(z) if memory.should_try(&plan.class) => {
                let c = z.compress(&buf)?;
                let worth_it = (c.len() as f64) < buf.len() as f64 * (1.0 - MIN_SAVING);
                memory.record(&plan.class, worth_it);
                worth_it.then_some(c)
            }
            _ => None,
        };
        let segs: Vec<Seg> = plan.segs.iter().map(|(s, _)| *s).collect();
        let (codec, data) = match &packed {
            Some(c) => (Codec::Zstd, c.as_slice()),
            None => (Codec::None, buf.as_slice()),
        };
        let frame = Chunk { codec, raw_len: plan.len as u32, segs, data }.encode();
        if frames.send(frame).is_err() {
            return Err(Error::Io(std::io::ErrorKind::BrokenPipe.into()));
        }
    }
    Ok(())
}

/// Appends one segment of a file to `buf`.
fn read_segment(path: &std::path::Path, seg: &Seg, buf: &mut Vec<u8>) -> Result<()> {
    let mut f = File::open(path)?;
    if seg.offset > 0 {
        f.seek(SeekFrom::Start(seg.offset))?;
    }
    let start = buf.len();
    buf.resize(start + seg.len as usize, 0);
    f.read_exact(&mut buf[start..])
        .map_err(|e| Error::Io(std::io::Error::new(e.kind(), format!("{} changed while sending: {e}", path.display()))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gives_up_on_incompressible_types_then_reprobes() {
        let m = ClassMemory::default();
        for _ in 0..GIVE_UP_AFTER {
            assert!(m.should_try("mp4"));
            m.record("mp4", false);
        }
        let tries: Vec<bool> = (0..REPROBE_EVERY).map(|_| m.should_try("mp4")).collect();
        assert_eq!(tries.iter().filter(|t| **t).count(), 1, "one re-probe per {REPROBE_EVERY} chunks");
        assert!(tries[REPROBE_EVERY as usize - 1]);
        assert!(m.should_try("txt"), "other types are unaffected");
    }

    #[test]
    fn a_success_resets_the_count() {
        let m = ClassMemory::default();
        m.record("pak", false);
        m.record("pak", false);
        m.record("pak", true);
        m.record("pak", false);
        assert!(m.should_try("pak"));
    }
}
