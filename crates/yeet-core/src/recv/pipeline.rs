//! One connection's worth of receiving: read frames, write files in parallel, send ACKs.

use super::files::Files;
use super::paths::Placer;
use super::session::State;
use crate::protocol::{self as p, Chunk, Codec, Kind, Totals};
use crate::{Error, Live, Report, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

const ACK_EVERY: Duration = Duration::from_millis(300);

/// Chunks waiting for a writer, at most. Bounds memory to about this many times 4 MiB.
const QUEUE: usize = 32;

/// How long to wait for the sender to hang up after SUMMARY.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// What a writer does next.
enum Work {
    /// A CHUNK payload.
    Chunk(Vec<u8>),
    /// Copy this file (by id) from a path on this computer.
    Local(u64, PathBuf),
}

/// Receives until DONE, then confirms with SUMMARY. LOCAL frames are accepted only with `local`.
pub(super) fn run(
    stream: &TcpStream,
    r: &mut BufReader<&TcpStream>,
    state: &mut State,
    threads: usize,
    local: bool,
    live: &Live,
) -> Result<Report> {
    let (tx, rx) = bounded::<Work>(QUEUE);
    let stop_acks = AtomicBool::new(false);
    let State { placer, files, disk } = state;
    let max_file = disk.max_file();
    let files = &*files;
    let (read, made, written, acked) = std::thread::scope(|s| {
        let acker = s.spawn(|| ack_loop(stream, live, &stop_acks));
        let (mtx, mrx) = crossbeam_channel::unbounded::<(PathBuf, bool)>();
        let maker = s.spawn(move || make_meta(mrx));
        let writers: Vec<_> = (0..threads)
            .map(|_| {
                let rx = rx.clone();
                s.spawn(move || writer(rx, files, live))
            })
            .collect();
        drop(rx);
        #[cfg(feature = "timing")]
        let t0 = Instant::now();
        let read = read_loop(r, placer, files, max_file, local, tx, mtx, live);
        #[cfg(feature = "timing")]
        eprintln!("[r] read done {:?}", t0.elapsed());
        let made = maker.join().expect("maker panicked");
        #[cfg(feature = "timing")]
        eprintln!("[r] meta done {:?}", t0.elapsed());
        let written: Result<()> = writers.into_iter().try_for_each(|w| w.join().expect("writer panicked"));
        #[cfg(feature = "timing")]
        eprintln!("[r] writers done {:?}", t0.elapsed());
        stop_acks.store(true, Relaxed);
        let acked = acker.join().expect("acker panicked");
        (read, made, written, acked)
    });
    read?;
    made?;
    written?;
    acked?;
    let missing = files.incomplete();
    if missing > 0 {
        return Err(Error::protocol(format!("sender finished with {missing} files incomplete")));
    }
    let totals = Totals { files: live.files_done.load(Relaxed), bytes: live.bytes_done.load(Relaxed) };
    (&*stream).write_all(&totals.encode(Kind::Summary))?;
    close_gracefully(stream);
    Ok(Report { files: totals.files, bytes: totals.bytes, wire: 0, secs: 0.0 })
}

/// Lets the sender read SUMMARY before the connection goes away. If unread data is left when a
/// socket closes, Windows resets the connection, and the reset makes the sender's side throw away
/// SUMMARY too: the sender then reports a failure for a transfer that completed.
fn close_gracefully(stream: &TcpStream) {
    let _ = stream.shutdown(Shutdown::Write);
    let _ = stream.set_read_timeout(Some(CLOSE_TIMEOUT));
    let mut sink = [0u8; 4096];
    while matches!((&*stream).read(&mut sink), Ok(n) if n > 0) {}
}

/// Sends progress regularly, and CANCEL if this side cancels.
fn ack_loop(stream: &TcpStream, live: &Live, stop: &AtomicBool) -> Result<()> {
    while !stop.load(Relaxed) {
        if live.cancelled() {
            let _ = (&*stream).write_all(&p::empty(Kind::Cancel));
            return Err(Error::Cancelled);
        }
        let progress = Totals { files: live.files_done.load(Relaxed), bytes: live.bytes_done.load(Relaxed) };
        (&*stream).write_all(&progress.encode(Kind::Ack))?;
        let t = Instant::now();
        while t.elapsed() < ACK_EVERY && !stop.load(Relaxed) && !live.cancelled() {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    Ok(())
}

/// Handles metadata itself and passes chunks to the writers. Returns at DONE; dropping `chunks`
/// on return lets the writers finish.
fn read_loop(
    r: &mut BufReader<&TcpStream>,
    placer: &mut Placer,
    files: &Files,
    max_file: u64,
    local: bool,
    work: Sender<Work>,
    meta: Sender<(PathBuf, bool)>,
    live: &Live,
) -> Result<()> {
    loop {
        if live.cancelled() {
            return Err(Error::Cancelled);
        }
        let (kind, payload) = p::read_frame(r)?;
        match kind {
            Kind::Dir => {
                let path = placer.place(p::Cursor::new(&payload).rest_str()?)?;
                if placer.mark_dir(&path) {
                    let _ = meta.send((path, true));
                }
            }
            Kind::File => {
                let (id, size, rel) = p::parse_file(&payload)?;
                if size > max_file {
                    return Err(Error::Io(std::io::Error::new(
                        std::io::ErrorKind::FileTooLarge,
                        format!("{rel} is 4 GB or larger, which this drive's FAT format can't hold"),
                    )));
                }
                let path = placer.place(rel)?;
                if size == 0 {
                    let _ = meta.send((path.clone(), false)); // no chunk will ever mention it
                    live.files_done.fetch_add(1, Relaxed);
                }
                files.insert(id, path, size);
            }
            Kind::Chunk => {
                if work.send(Work::Chunk(payload)).is_err() {
                    return Err(Error::Io(std::io::ErrorKind::BrokenPipe.into()));
                }
            }
            Kind::Local if local => {
                let (id, src) = p::parse_local(&payload)?;
                let src = PathBuf::from(src);
                if !src.is_absolute() {
                    return Err(Error::protocol("LOCAL path must be absolute"));
                }
                if work.send(Work::Local(id, src)).is_err() {
                    return Err(Error::Io(std::io::ErrorKind::BrokenPipe.into()));
                }
            }
            Kind::Done => return Ok(()),
            Kind::Cancel => return Err(Error::CancelledByPeer),
            other => return Err(Error::protocol(format!("unexpected {other:?} frame from sender"))),
        }
    }
}

/// Creates folders, and empty files (which no chunk mentions). Writers create missing folders
/// themselves too, so nothing waits on this thread.
fn make_meta(rx: Receiver<(PathBuf, bool)>) -> Result<()> {
    for (path, dir) in rx {
        #[cfg(feature = "timing")]
        let t0 = Instant::now();
        #[cfg(feature = "timing")]
        let _slow = SlowLog(t0, path.clone());
        if dir {
            std::fs::create_dir_all(&path)?;
        } else {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            File::create(&path)?;
        }
    }
    Ok(())
}

#[cfg(feature = "timing")]
struct SlowLog(Instant, PathBuf);
#[cfg(feature = "timing")]
impl Drop for SlowLog {
    fn drop(&mut self) {
        if self.0.elapsed() > Duration::from_millis(100) {
            eprintln!("[slow-meta] {:?} {}", self.0.elapsed(), self.1.display());
        }
    }
}

/// Copies a file on this computer, counting progress as it goes.
fn copy_local(id: u64, src: &std::path::Path, files: &Files, live: &Live) -> Result<()> {
    let slot = files.get(id).ok_or_else(|| Error::protocol("copy for unknown file"))?;
    let mut counted = 0u64;
    let wrote = slot.copy_from(src, &mut |done| {
        live.bytes_done.fetch_add(done.saturating_sub(counted), Relaxed);
        counted = counted.max(done);
        !live.cancelled()
    })?;
    if wrote.complete {
        // Small copies may finish without a progress report for the last bytes.
        live.bytes_done.fetch_add(wrote.bytes.saturating_sub(counted), Relaxed);
        live.files_done.fetch_add(1, Relaxed);
    }
    Ok(())
}

/// Decompresses chunks and writes their segments, or copies local files.
fn writer(work: Receiver<Work>, files: &Files, live: &Live) -> Result<()> {
    let mut zstd = zstd::bulk::Decompressor::new()?;
    #[cfg(feature = "timing")]
    let start = Instant::now();
    #[cfg(feature = "timing")]
    let mut busy = Duration::ZERO;
    #[cfg(feature = "timing")]
    let mut last = Duration::ZERO;
    for item in work {
        #[cfg(feature = "timing")]
        let t0 = Instant::now();

        if live.cancelled() {
            return Err(Error::Cancelled);
        }
        let payload = match item {
            Work::Chunk(p) => p,
            Work::Local(id, src) => {
                copy_local(id, &src, files, live)?;
                continue;
            }
        };
        let chunk = Chunk::decode(&payload)?;
        let decoded;
        let data: &[u8] = match chunk.codec {
            Codec::None => chunk.data,
            Codec::Zstd => {
                decoded = zstd.decompress(chunk.data, chunk.raw_len as usize)?;
                &decoded
            }
        };
        if data.len() != chunk.raw_len as usize {
            return Err(Error::protocol("chunk size mismatch"));
        }
        let mut at = 0usize;
        for seg in &chunk.segs {
            let slot = files.get(seg.id).ok_or_else(|| Error::protocol("data for unknown file"))?;
            if seg.offset + seg.len as u64 > slot.size() {
                return Err(Error::protocol("segment past end of file"));
            }
            let piece = &data[at..at + seg.len as usize];
            at += seg.len as usize;
            let wrote = slot.write(seg.offset, piece)?;
            live.bytes_done.fetch_add(wrote.bytes, Relaxed);
            if wrote.complete {
                live.files_done.fetch_add(1, Relaxed);
            }
        }
        #[cfg(feature = "timing")]
        {
            busy += t0.elapsed();
        }
        #[cfg(feature = "timing")]
        {
            last = start.elapsed();
        }
    }
    #[cfg(feature = "timing")]
    if last > Duration::from_secs(4) || busy > Duration::from_secs(3) {
        eprintln!("[writer] busy {busy:?} last chunk done at {last:?}");
    }
    Ok(())
}
