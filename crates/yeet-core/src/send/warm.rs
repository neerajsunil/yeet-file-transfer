//! Reading ahead while nothing else is happening: between choosing what to send and the other
//! side accepting, the disk would otherwise sit idle.

use super::scan::Entry;
use std::fs::File;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

const BUF: usize = 1 << 20;

/// Reads files in scan order (the order they'll be sent in) and throws the data away, so the OS
/// has it cached when the packers ask. Stops after `budget` bytes, or as soon as `stop` is set:
/// once sending starts, reading ahead would only compete with it for the disk.
pub(super) fn warm(entries: &[Entry], budget: u64, stop: &AtomicBool) {
    let mut buf = vec![0u8; BUF];
    let mut left = budget;
    for e in entries.iter().filter(|e| !e.dir && e.size > 0) {
        let Ok(mut f) = File::open(&e.path) else { continue };
        loop {
            if stop.load(Relaxed) || left == 0 {
                return;
            }
            let want = BUF.min(left as usize);
            match f.read(&mut buf[..want]) {
                Ok(0) | Err(_) => break,
                Ok(n) => left -= n as u64,
            }
        }
    }
}
