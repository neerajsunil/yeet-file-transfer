//! Writing received files.
//!
//! Each file is written strictly front to back: segments that arrive ahead of the write
//! position are parked in memory until the gap before them is filled. Writing past the end of
//! valid data makes NTFS zero-fill the gap synchronously, which is very slow on HDDs, and a
//! clean prefix is what makes resuming exact.

use super::copy;
use crate::protocol::SMALL;
use crate::{Error, Result};
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// One file being received.
pub(super) struct Slot {
    path: PathBuf,
    size: u64,
    /// Reserve the full size when creating the file (not on FAT, which zero-fills it).
    preallocate: bool,
    state: Mutex<SlotState>,
}

#[derive(Default)]
struct SlotState {
    file: Option<File>,
    /// Offset of the first byte not yet written. The file is complete when this equals `size`.
    next: u64,
    /// Segments that arrived ahead of `next`, by offset.
    pending: BTreeMap<u64, Vec<u8>>,
    /// A writer currently owns the file and is writing.
    busy: bool,
}

/// What a write did.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Wrote {
    /// Bytes that reached the file now (parked data doesn't count until it's written).
    pub bytes: u64,
    /// This write finished the file.
    pub complete: bool,
}

impl Slot {
    pub fn new(path: PathBuf, size: u64, preallocate: bool) -> Slot {
        Slot { path, size, preallocate, state: Mutex::default() }
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.state.lock().expect("slot poisoned")
    }

    fn open(&self, resuming: bool) -> Result<File> {
        if resuming {
            return Ok(OpenOptions::new().write(true).open(&self.path)?);
        }
        let create = || OpenOptions::new().write(true).create(true).truncate(true).open(&self.path);
        let f = match create() {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some(parent) = self.path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                create()?
            }
            r => r?,
        };
        if self.preallocate && self.size >= SMALL {
            f.set_len(self.size)?; // reserve space up front to limit fragmentation
        }
        Ok(f)
    }

    /// Writes `data` at `offset` if it's next in line, then any parked segments that follow.
    /// Otherwise parks a copy for whichever writer fills the gap.
    pub fn write(&self, offset: u64, data: &[u8]) -> Result<Wrote> {
        let (mut file, resuming) = {
            let mut st = self.lock();
            if st.busy || offset != st.next {
                if offset >= st.next {
                    st.pending.insert(offset, data.to_vec());
                }
                return Ok(Wrote { bytes: 0, complete: false });
            }
            st.busy = true;
            (st.file.take(), st.next > 0)
        };
        let mut parked: Option<Vec<u8>> = None;
        let mut off = offset;
        loop {
            let buf = parked.as_deref().unwrap_or(data);
            #[cfg(feature = "timing")] let t0 = std::time::Instant::now();
            #[cfg(feature = "timing")] let opened = file.is_some();
            let written = match file.as_mut() {
                Some(f) => write_all_at(f, buf, off),
                None => self.open(resuming).and_then(|f| write_all_at(file.insert(f), buf, off)),
            };
            #[cfg(feature = "timing")] if t0.elapsed() > std::time::Duration::from_millis(100) {
                eprintln!("[slow] {:?} open={} {} bytes {}", t0.elapsed(), !opened, buf.len(), self.path.display());
            }
            let mut st = self.lock();
            if let Err(e) = written {
                st.busy = false;
                st.file = file;
                return Err(e);
            }
            off += buf.len() as u64;
            st.next = off;
            if off == self.size {
                close_later(file);
                return Ok(Wrote { bytes: off - offset, complete: true });
            }
            match st.pending.remove(&off) {
                Some(d) => parked = Some(d),
                None => {
                    st.busy = false;
                    st.file = file;
                    return Ok(Wrote { bytes: off - offset, complete: false });
                }
            }
        }
    }
}

impl Slot {
    /// Fills the whole file by copying `src` on this computer. `progress` is as for
    /// [`copy::copy_file`]. A failed or cancelled copy leaves the file counted as empty, so a
    /// resume copies it again from the start.
    pub fn copy_from(&self, src: &Path, progress: &mut dyn FnMut(u64) -> bool) -> Result<Wrote> {
        {
            let mut st = self.lock();
            if st.busy || st.next == self.size {
                return Ok(Wrote { bytes: 0, complete: false }); // already being written, or done
            }
            st.busy = true;
            st.pending.clear();
            drop(st.file.take()); // the copy replaces the file
        }
        let copied = (|| {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            copy::copy_file(src, &self.path, progress)?;
            if std::fs::metadata(&self.path)?.len() != self.size {
                return Err(Error::protocol(format!("{} changed while sending", src.display())));
            }
            Ok(())
        })();
        let mut st = self.lock();
        st.busy = false;
        st.next = if copied.is_ok() { self.size } else { 0 };
        copied.map(|()| Wrote { bytes: self.size, complete: true })
    }
}

/// Closing a file on Windows waits for antivirus to scan it, so closes happen on their own
/// threads and writers move straight on to the next file.
fn close_later(file: Option<File>) {
    static TX: std::sync::OnceLock<crossbeam_channel::Sender<File>> = std::sync::OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, rx) = crossbeam_channel::unbounded::<File>();
        for _ in 0..16 {
            let rx = rx.clone();
            std::thread::spawn(move || rx.iter().for_each(drop));
        }
        tx
    });
    if let Some(f) = file {
        let _ = tx.send(f);
    }
}

/// How far along one file is, as reported to a resuming sender.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Progress {
    pub id: u64,
    pub written: u64,
    pub complete: bool,
}

/// All files of a session by sender-assigned id, including completed ones.
pub(super) struct Files {
    slots: Mutex<HashMap<u64, Arc<Slot>>>,
    preallocate: bool,
}

impl Files {
    pub fn new(preallocate: bool) -> Files {
        Files { slots: Mutex::default(), preallocate }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Arc<Slot>>> {
        self.slots.lock().expect("files poisoned")
    }

    pub fn insert(&self, id: u64, path: PathBuf, size: u64) {
        self.lock().insert(id, Arc::new(Slot::new(path, size, self.preallocate)));
    }

    pub fn get(&self, id: u64) -> Option<Arc<Slot>> {
        self.lock().get(&id).cloned()
    }

    /// Drops data parked by a dead connection and reports how far each file got.
    pub fn prepare_resume(&self) -> Vec<Progress> {
        self.lock()
            .iter()
            .map(|(id, s)| {
                let mut st = s.lock();
                st.pending.clear();
                st.busy = false;
                Progress { id: *id, written: st.next, complete: st.next == s.size }
            })
            .collect()
    }

    pub fn incomplete(&self) -> usize {
        self.lock().values().filter(|s| s.lock().next < s.size).count()
    }

    /// Deletes files that never finished; completed ones are kept.
    pub fn remove_partials(&self) {
        for slot in self.lock().values() {
            let mut st = slot.lock();
            if st.next < slot.size {
                drop(st.file.take()); // Windows can't delete open files
                let _ = std::fs::remove_file(&slot.path);
            }
        }
    }
}

#[cfg(windows)]
fn write_all_at(f: &File, mut buf: &[u8], mut offset: u64) -> Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_write(buf, offset)?;
        if n == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::WriteZero).into());
        }
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

#[cfg(unix)]
fn write_all_at(f: &File, buf: &[u8], offset: u64) -> Result<()> {
    Ok(std::os::unix::fs::FileExt::write_all_at(f, buf, offset)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_order_segments_are_parked_and_written_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f");
        let slot = Slot::new(path.clone(), 9, true);
        assert_eq!(slot.write(6, b"ghi").unwrap(), Wrote { bytes: 0, complete: false });
        assert_eq!(slot.write(3, b"def").unwrap(), Wrote { bytes: 0, complete: false });
        assert_eq!(slot.write(0, b"abc").unwrap(), Wrote { bytes: 9, complete: true });
        assert_eq!(std::fs::read(path).unwrap(), b"abcdefghi");
    }

    #[test]
    fn resume_reports_the_clean_prefix_and_partials_are_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let files = Files::new(true);
        files.insert(0, tmp.path().join("done"), 2);
        files.insert(1, tmp.path().join("half"), 4);
        files.get(0).unwrap().write(0, b"ok").unwrap();
        files.get(1).unwrap().write(0, b"ab").unwrap();
        files.get(1).unwrap().write(3, b"d").unwrap(); // parked; lost when the connection drops

        let mut p = files.prepare_resume();
        p.sort_by_key(|p| p.id);
        assert_eq!(
            p,
            [Progress { id: 0, written: 2, complete: true }, Progress { id: 1, written: 2, complete: false }]
        );
        assert_eq!(files.incomplete(), 1);

        files.remove_partials();
        assert!(tmp.path().join("done").exists());
        assert!(!tmp.path().join("half").exists());
    }

    #[test]
    fn resumed_writes_continue_the_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let files = Files::new(true);
        files.insert(0, tmp.path().join("f"), 4);
        files.get(0).unwrap().write(0, b"ab").unwrap();
        files.prepare_resume();
        assert!(files.get(0).unwrap().write(2, b"cd").unwrap().complete);
        assert_eq!(std::fs::read(tmp.path().join("f")).unwrap(), b"abcd");
    }
}
