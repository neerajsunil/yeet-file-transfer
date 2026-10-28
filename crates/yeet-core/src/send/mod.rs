//! Sending side.
//!
//! ```text
//! scan                 walk the sources once, for the offer's totals and for progress
//! planner (1 thread)   emit DIR/FILE frames and cut file data into chunk plans
//!   -> packers (N)     read each plan's segments, compress if worthwhile, encode a CHUNK
//!   -> writer (1)      write frames to the socket, optionally rate-limited
//! reader (1 thread)    take ACKs (progress + heartbeat), CANCEL and the final SUMMARY
//! ```
//!
//! Metadata goes straight from the planner to the writer, ahead of any chunk that refers to it,
//! so the receiver always learns about a file before its data arrives.
//!
//! If the connection drops, [`send`] reconnects (re-resolving the peer's address, which may have
//! changed) and resumes: the receiver says how much of each file it has, and only the rest is sent.

mod conn;
mod pack;
mod plan;
mod scan;
mod warm;

use crate::disk::{self, Disk};
use crate::net::{connect, same_host, tune_socket, with_cancel_watch};
use crate::protocol::{self as p, Kind};
use crate::{Error, Live, Report, Result};
use crossbeam_channel::{bounded, Receiver};
use std::collections::HashMap;
use std::io::{self, Write};
use std::net::{Shutdown, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compress {
    None,
    /// Like [`Compress::Zstd`] when the data crosses a network; off when the receiver is this
    /// same computer, where there's no wire to save and the CPU is better spent reading.
    Auto(i32),
    /// Try zstd at this level, but stop trying on file types that keep failing to shrink.
    Zstd(i32),
}

/// What a send has learned before it starts: the file list, and a head start on reading.
/// Made with [`prepare`] while the person is still choosing a device.
pub struct Prepared {
    sources: Vec<PathBuf>,
    scanned: Mutex<Option<Receiver<Result<Arc<Vec<scan::Entry>>>>>>,
    stop_warming: Arc<AtomicBool>,
}

impl Drop for Prepared {
    fn drop(&mut self) {
        self.stop_warming.store(true, Relaxed);
    }
}

/// Starts scanning `sources`, probing their drives and reading the first part of them into the
/// OS cache, all in the background. Pass the result in [`SendOpts::prepared`]; it's used if the
/// sources still match. Dropping it stops the reading.
pub fn prepare(sources: Vec<PathBuf>) -> Prepared {
    let (tx, rx) = bounded(1);
    let stop = Arc::new(AtomicBool::new(false));
    let (srcs, stop2) = (sources.clone(), stop.clone());
    std::thread::spawn(move || {
        Disk::probe_all(&srcs);
        let scanned = scan::scan(&srcs).map(Arc::new);
        let entries = scanned.as_ref().ok().cloned();
        let _ = tx.send(scanned);
        if let Some(e) = entries {
            warm::warm(&e, WARM_BUDGET, &stop2);
        }
    });
    Prepared { sources, scanned: Mutex::new(Some(rx)), stop_warming: stop }
}

/// Returns the peer's current addresses, best first. Called again before every reconnect.
pub type Resolve = Arc<dyn Fn() -> Vec<SocketAddr> + Send + Sync>;

pub struct SendOpts {
    pub sources: Vec<PathBuf>,
    pub resolve: Resolve,
    /// This device's name, shown to the receiver.
    pub name: String,
    /// Payload bytes per second; `None` is unlimited.
    pub limit: Option<f64>,
    pub compress: Compress,
    pub threads: usize,
    /// Work done ahead of time by [`prepare`], if any.
    pub prepared: Option<Prepared>,
    /// When the receiver is this same computer, let it copy the files directly instead of
    /// streaming them through a socket.
    pub local: bool,
}

/// How much to read ahead into the OS cache while waiting for the receiver to accept.
const WARM_BUDGET: u64 = 512 << 20;

/// How long to wait for the person on the other side to accept.
const OFFER_TIMEOUT: Duration = Duration::from_secs(120);
/// No ACK for this long means the connection is dead.
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);
/// Keep trying to reconnect for this long after a drop.
const RECONNECT_FOR: Duration = Duration::from_secs(300);
const RECONNECT_PAUSE: Duration = Duration::from_secs(1);
/// How long to spend telling a receiver about a cancel it missed.
const NOTIFY_TIMEOUT: Duration = Duration::from_secs(2);

/// Sends `o.sources` to the peer, blocking until done. Progress and cancellation go through `live`.
pub fn send(o: &SendOpts, live: &Live) -> Result<Report> {
    #[cfg(feature = "timing")]
    let t0 = Instant::now();
    let prepared = o.prepared.as_ref().filter(|p| p.sources == o.sources);
    let early = prepared.and_then(|p| p.scanned.lock().expect("poisoned").take()).and_then(|rx| rx.recv().ok());
    let entries = match early {
        Some(Ok(e)) => e,
        _ => Arc::new(scan::scan(&o.sources)?),
    };
    #[cfg(feature = "timing")]
    eprintln!("[t] scan {:?}", t0.elapsed());
    // Read ahead while the person on the other side decides, unless `prepare` already is.
    let warming = match prepared {
        Some(p) => p.stop_warming.clone(),
        None => {
            let stop = Arc::new(AtomicBool::new(false));
            let (e, s) = (entries.clone(), stop.clone());
            std::thread::spawn(move || warm::warm(&e, WARM_BUDGET, &s));
            stop
        }
    };
    let _stop_warming = StopOnDrop(&warming);
    let src_disk = Disk::probe_all(&o.sources);
    let tuning = Tuning { src_disk, warming: &warming };
    live.files_total.store(entries.iter().filter(|e| !e.dir).count() as u64, Relaxed);
    live.bytes_total.store(entries.iter().map(|e| e.size).sum(), Relaxed);
    let offer = p::Offer {
        transfer: fastrand::u64(..),
        name: o.name.clone(),
        files: live.files_total.load(Relaxed),
        bytes: live.bytes_total.load(Relaxed),
        items: o.sources.iter().filter_map(|s| s.file_name()).map(|n| n.to_string_lossy().into_owned()).collect(),
    };
    let start = Instant::now();
    let mut dropped_at: Option<Instant> = None;
    let mut resuming = false;
    loop {
        if live.cancelled() {
            return Err(cancelled(o, &offer, resuming));
        }
        let failure = match attempt(o, live, &entries, &offer, resuming, &tuning) {
            Ok(summary) => {
                live.reconnecting.store(false, Relaxed);
                let wire = live.wire_bytes.load(Relaxed);
                return Ok(Report {
                    files: summary.files,
                    bytes: summary.bytes,
                    wire,
                    secs: start.elapsed().as_secs_f64(),
                });
            }
            Err(f) => f,
        };
        if live.cancelled() {
            let unaware = (failure.accepted || resuming) && !failure.told_peer;
            return Err(cancelled(o, &offer, unaware));
        }
        // Before the first acceptance there's nothing to resume; report the problem directly.
        if !failure.accepted && !resuming {
            return Err(failure.error);
        }
        if failure.accepted {
            resuming = true;
            dropped_at = Some(Instant::now());
        }
        let since = *dropped_at.get_or_insert_with(Instant::now);
        if !failure.error.is_disconnect() || since.elapsed() > RECONNECT_FOR {
            return Err(failure.error);
        }
        live.reconnecting.store(true, Relaxed);
        if sleep_unless_cancelled(RECONNECT_PAUSE, live).is_err() {
            return Err(cancelled(o, &offer, true));
        }
    }
}

/// Finishes a cancelled send. If the receiver is holding the transfer for a resume and never got
/// CANCEL (the connection was dead or stuck), tells it on a fresh connection. Otherwise it would
/// show "reconnecting" until its resume window runs out.
fn cancelled(o: &SendOpts, offer: &p::Offer, receiver_unaware: bool) -> Error {
    if receiver_unaware {
        notify_cancel(o, offer);
    }
    Error::Cancelled
}

/// Best effort: reconnects as the same transfer, which the receiver resumes without asking,
/// then sends CANCEL and waits for the receiver to hang up.
fn notify_cancel(o: &SendOpts, offer: &p::Offer) {
    let Ok(stream) = connect(&(o.resolve)(), &Live::default()) else { return };
    let mut msg = p::MAGIC.to_vec();
    msg.extend(offer.encode());
    msg.extend(p::empty(Kind::Cancel));
    let _ = stream.set_write_timeout(Some(NOTIFY_TIMEOUT));
    let _ = stream.set_read_timeout(Some(NOTIFY_TIMEOUT));
    if (&stream).write_all(&msg).is_ok() {
        // Closing first could reset the connection before the receiver reads CANCEL.
        let _ = io::copy(&mut &stream, &mut io::sink());
    }
}

/// Why one connection ended, and whether the receiver had accepted on it (i.e. whether there's
/// now a transfer to resume).
struct Failure {
    error: Error,
    accepted: bool,
    /// CANCEL reached the socket, so the receiver knows the transfer is over.
    told_peer: bool,
}

fn before_accept(e: impl Into<Error>) -> Failure {
    Failure { error: e.into(), accepted: false, told_peer: false }
}

fn after_accept(e: impl Into<Error>) -> Failure {
    Failure { error: e.into(), accepted: true, told_peer: false }
}

/// What the send knows about its own side, for pacing.
struct Tuning<'a> {
    src_disk: Disk,
    /// Set to stop reading ahead once the real reading starts.
    warming: &'a AtomicBool,
}

struct StopOnDrop<'a>(&'a AtomicBool);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Relaxed);
    }
}

/// One connection's worth of work.
fn attempt(
    o: &SendOpts,
    live: &Live,
    entries: &[scan::Entry],
    offer: &p::Offer,
    resuming: bool,
    tuning: &Tuning,
) -> std::result::Result<p::Totals, Failure> {
    let stream = connect(&(o.resolve)(), live).map_err(before_accept)?;
    tune_socket(&stream).map_err(before_accept)?;
    (&stream).write_all(p::MAGIC).map_err(before_accept)?;
    (&stream).write_all(&offer.encode()).map_err(before_accept)?;
    // A resume is accepted automatically, so it doesn't need the long wait for a person.
    let wait = if resuming { HEARTBEAT_TIMEOUT } else { OFFER_TIMEOUT };
    let accept = conn::wait_for_answer(&stream, live, wait)
        .map_err(|e| Failure { told_peer: matches!(e, Error::Cancelled), ..before_accept(e) })?;
    live.accepted.store(true, Relaxed);
    live.reconnecting.store(false, Relaxed);
    tuning.warming.store(true, Relaxed);

    let here = same_host(&stream);
    // Same computer, and the receiver agrees: it copies the files itself, nothing goes through
    // the socket but paths. A rate limit means someone wants the real network path exercised.
    let local =
        o.local && here && accept.local && o.limit.is_none() && entries.iter().all(|e| e.path.to_str().is_some());
    let compress = match o.compress {
        Compress::Auto(_) if here => Compress::None,
        Compress::Auto(level) => Compress::Zstd(level),
        c => c,
    };
    let readers = if local { 0 } else { disk::readers(&tuning.src_disk, o.threads) };
    // A spinning disk on either end wants data in the order it lies on disk.
    let in_order = tuning.src_disk.spinning() || accept.disk.spinning();

    stream.set_read_timeout(Some(HEARTBEAT_TIMEOUT)).map_err(after_accept)?;
    let (kind, payload) = p::read_frame(&mut &stream).map_err(after_accept)?;
    if kind != Kind::Have {
        return Err(after_accept(Error::protocol("expected HAVE")));
    }
    let have: HashMap<u64, u64> = p::parse_have(&payload).map_err(after_accept)?.into_iter().collect();
    live.bytes_done.store(have.values().sum(), Relaxed);

    let totals = p::Totals { files: offer.files, bytes: offer.bytes };
    let memory = pack::ClassMemory::default();
    let (frame_tx, frame_rx) = bounded::<Vec<u8>>(o.threads * 2);
    let (plan_tx, plan_rx) = bounded::<plan::Plan>(o.threads * 2);

    let mut told_peer = false;
    let result = with_cancel_watch(&stream, live, || {
        std::thread::scope(|s| {
            let reader = s.spawn(|| {
                let r = conn::read_acks(&stream, live);
                if r.is_err() {
                    let _ = stream.shutdown(Shutdown::Both); // wake the writer
                }
                r
            });
            let writer = s.spawn(|| conn::write_frames(&stream, frame_rx, o.limit, live, totals));
            let packers: Vec<_> = (0..readers)
                .map(|_| {
                    let (rx, tx, memory) = (plan_rx.clone(), frame_tx.clone(), &memory);
                    s.spawn(move || pack::packer(rx, tx, compress, memory))
                })
                .collect();
            drop(plan_rx);
            #[cfg(feature = "timing")]
            let t0 = Instant::now();
            let planned = if local {
                drop(plan_tx);
                plan::plan_local(entries, &have, frame_tx)
            } else {
                plan::plan(entries, &have, in_order, plan_tx, frame_tx)
            };
            #[cfg(feature = "timing")]
            eprintln!("[t] planned {:?}", t0.elapsed());
            let packed: Result<()> = packers.into_iter().try_for_each(|w| w.join().expect("packer panicked"));
            #[cfg(feature = "timing")]
            eprintln!("[t] packed {:?}", t0.elapsed());
            let written = writer.join().expect("writer panicked");
            #[cfg(feature = "timing")]
            eprintln!("[t] written {:?}", t0.elapsed());
            told_peer = matches!(written, Err(Error::Cancelled));
            let read = reader.join().expect("reader panicked");
            // The reader sees the root cause (CANCEL, timeout) when the connection dies.
            let summary = read?;
            written?;
            planned?;
            packed?;
            Ok(summary)
        })
    });
    result.map_err(|e| {
        let error = if live.cancelled() { Error::Cancelled } else { e };
        Failure { error, accepted: true, told_peer }
    })
}

fn sleep_unless_cancelled(d: Duration, live: &Live) -> Result<()> {
    let until = Instant::now() + d;
    while Instant::now() < until {
        if live.cancelled() {
            return Err(Error::Cancelled);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}
