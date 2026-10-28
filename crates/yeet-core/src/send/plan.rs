//! Decides what goes in each chunk.

use super::scan::Entry;
use crate::protocol::{self as p, Seg, BLOCK, SMALL};
use crate::{Error, Result};
use crossbeam_channel::Sender;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Most files in one bundle. The receiver creates a bundle's files one after another on one
/// thread, and creating a file costs about a millisecond on Windows, so a 4 MiB bundle of tiny
/// files would keep one thread busy for seconds while the others sit idle. Also keeps the count
/// within the u16 the chunk header has for it.
const BUNDLE_FILES: usize = 256;

/// The contents of one future chunk.
#[derive(Debug)]
pub(super) struct Plan {
    /// File type, for adaptive compression ("small" for bundles of small files).
    pub class: String,
    pub segs: Vec<(Seg, Arc<PathBuf>)>,
    pub len: usize,
}

/// Sends metadata frames to the writer and chunk plans to the packers, for everything the
/// receiver doesn't already have. `have` maps file id to bytes already written.
///
/// Files get ids in scan order, which is stable, so ids match across reconnects. With
/// `in_order`, chunks also follow scan order, which is roughly where files lie on disk.
pub(super) fn plan(
    entries: &[Entry],
    have: &HashMap<u64, u64>,
    in_order: bool,
    plans: Sender<Plan>,
    frames: Sender<Vec<u8>>,
) -> Result<()> {
    let closed = || Error::Io(std::io::ErrorKind::BrokenPipe.into());
    let new_bundle = || Plan { class: "small".into(), segs: Vec::new(), len: 0 };
    let mut bundle = new_bundle();
    // Small-file bundles are slow to read but light on the wire, big-file blocks the opposite.
    // Collected apart and then interleaved, so disk and network stay busy at the same time.
    let (mut small, mut big) = (Vec::new(), Vec::new());
    let files = entries.iter().filter(|e| !e.dir);
    for e in entries.iter().filter(|e| e.dir) {
        frames.send(p::dir(&e.rel)).map_err(|_| closed())?;
    }
    for (id, e) in (0u64..).zip(files) {
        let from = match have.get(&id) {
            Some(&n) if n >= e.size => continue, // already complete
            Some(&n) => n,                       // known to the receiver, partly written
            None => {
                frames.send(p::file(id, e.size, &e.rel)).map_err(|_| closed())?;
                0
            }
        };
        if e.size == 0 {
            continue;
        }
        let path = Arc::new(e.path.clone());
        if e.size < SMALL && from == 0 {
            bundle.segs.push((Seg { id, offset: 0, len: e.size as u32 }, path));
            bundle.len += e.size as usize;
            if bundle.len >= BLOCK || bundle.segs.len() >= BUNDLE_FILES {
                small.push(std::mem::replace(&mut bundle, new_bundle()));
            }
            continue;
        }
        let class = path.extension().map_or("(none)".into(), |x| x.to_string_lossy().to_lowercase());
        let mut offset = from;
        // In order, the small files before this big one go first, and everything goes in one
        // list, so nothing is interleaved below.
        if in_order && !bundle.segs.is_empty() {
            small.push(std::mem::replace(&mut bundle, new_bundle()));
        }
        let out = if in_order { &mut small } else { &mut big };
        while offset < e.size {
            let len = (e.size - offset).min(BLOCK as u64) as u32;
            let segs = vec![(Seg { id, offset, len }, path.clone())];
            out.push(Plan { class: class.clone(), segs, len: len as usize });
            offset += len as u64;
        }
    }
    if !bundle.segs.is_empty() {
        small.push(bundle);
    }
    let total = |v: &[Plan]| v.iter().map(|p| p.len as f64).sum::<f64>().max(1.0);
    let (st, bt) = (total(&small), total(&big));
    let (mut sd, mut bd) = (0.0, 0.0);
    let (mut small, mut big) = (small.into_iter().peekable(), big.into_iter().peekable());
    loop {
        let take_small = match (small.peek(), big.peek()) {
            (None, None) => break,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            _ => sd / st <= bd / bt,
        };
        let p = if take_small { small.next() } else { big.next() }.expect("peeked");
        *if take_small { &mut sd } else { &mut bd } += p.len as f64;
        plans.send(p).map_err(|_| closed())?;
    }
    Ok(())
}

/// Like [`plan`], for a receiver on this same computer: instead of data, each file gets a LOCAL
/// frame naming where it is, and the receiver copies it. Paths must be valid Unicode.
pub(super) fn plan_local(entries: &[Entry], have: &HashMap<u64, u64>, frames: Sender<Vec<u8>>) -> Result<()> {
    let closed = || Error::Io(std::io::ErrorKind::BrokenPipe.into());
    for e in entries.iter().filter(|e| e.dir) {
        frames.send(p::dir(&e.rel)).map_err(|_| closed())?;
    }
    for (id, e) in (0u64..).zip(entries.iter().filter(|e| !e.dir)) {
        match have.get(&id) {
            Some(&n) if n >= e.size => continue,
            Some(_) => {}
            None => frames.send(p::file(id, e.size, &e.rel)).map_err(|_| closed())?,
        }
        if e.size == 0 {
            continue;
        }
        let path = std::path::absolute(&e.path)?;
        let path = path.to_str().ok_or_else(|| Error::protocol("path isn't valid Unicode"))?;
        frames.send(p::local(id, path)).map_err(|_| closed())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{read_frame, Kind};

    fn entry(rel: &str, size: u64, dir: bool) -> Entry {
        Entry { rel: rel.into(), path: PathBuf::from(rel), size, dir }
    }

    fn run(entries: &[Entry], have: &[(u64, u64)]) -> (Vec<Kind>, Vec<Plan>) {
        let (ptx, prx) = crossbeam_channel::unbounded();
        let (ftx, frx) = crossbeam_channel::unbounded();
        plan(entries, &have.iter().copied().collect(), false, ptx, ftx).unwrap();
        let kinds = frx.iter().map(|f| read_frame(&mut f.as_slice()).unwrap().0).collect();
        (kinds, prx.iter().collect())
    }

    #[test]
    fn bundles_small_files_and_splits_large_ones() {
        let big = BLOCK as u64 * 2 + 5;
        let entries =
            [entry("d", 0, true), entry("d/a", 10, false), entry("d/b", 20, false), entry("d/big.iso", big, false)];
        let (kinds, plans) = run(&entries, &[]);
        assert_eq!(kinds, [Kind::Dir, Kind::File, Kind::File, Kind::File]);
        // one bundle with both small files, interleaved ahead of the big file's three pieces
        let lens: Vec<usize> = plans.iter().map(|p| p.len).collect();
        assert_eq!(lens, [30, BLOCK, BLOCK, 5]);
        assert_eq!(plans[0].segs.len(), 2);
        assert_eq!(plans[1].class, "iso");
    }

    #[test]
    fn in_order_keeps_scan_order() {
        let big = BLOCK as u64 + 5;
        let entries = [entry("a", 10, false), entry("b.iso", big, false), entry("c", 20, false)];
        let (ptx, prx) = crossbeam_channel::unbounded();
        let (ftx, _frx) = crossbeam_channel::unbounded();
        plan(&entries, &HashMap::new(), true, ptx, ftx).unwrap();
        let ids: Vec<Vec<u64>> = prx.iter().map(|p| p.segs.iter().map(|s| s.0.id).collect()).collect();
        assert_eq!(ids, [vec![0], vec![1], vec![1], vec![2]]);
    }

    #[test]
    fn bundles_hold_a_limited_number_of_files() {
        let entries: Vec<Entry> = (0..BUNDLE_FILES * 2 + 1).map(|i| entry(&format!("f{i}"), 1, false)).collect();
        let (_, plans) = run(&entries, &[]);
        let counts: Vec<usize> = plans.iter().map(|p| p.segs.len()).collect();
        assert_eq!(counts, [BUNDLE_FILES, BUNDLE_FILES, 1]);
    }

    #[test]
    fn resumes_from_what_the_receiver_has() {
        let big = BLOCK as u64 + 100;
        let entries = [entry("done.txt", 10, false), entry("big.bin", big, false), entry("new.txt", 3, false)];
        // file 0 complete, file 1 written up to BLOCK, file 2 unknown to the receiver
        let (kinds, plans) = run(&entries, &[(0, 10), (1, BLOCK as u64)]);
        assert_eq!(kinds, [Kind::File], "only the unknown file needs metadata");
        let segs: Vec<Seg> = plans.iter().flat_map(|p| p.segs.iter().map(|s| s.0)).collect();
        assert_eq!(segs, [Seg { id: 2, offset: 0, len: 3 }, Seg { id: 1, offset: BLOCK as u64, len: 100 }]);
    }
}
