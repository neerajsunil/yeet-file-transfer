//! The sender's side of the conversation with the receiver.

use crate::protocol::{self as p, Kind, Totals};
use crate::{Error, Live, Result};
use crossbeam_channel::{Receiver, RecvTimeoutError};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

/// How quickly a waiting writer notices a cancel.
const CANCEL_POLL: Duration = Duration::from_millis(100);

/// Waits for ACCEPT or DECLINE, polling so a cancel can interrupt the wait. Returns what ACCEPT
/// says, and [`Error::Cancelled`] only if CANCEL was sent.
pub(super) fn wait_for_answer(stream: &TcpStream, live: &Live, timeout: Duration) -> Result<p::Accept> {
    stream.set_read_timeout(Some(Duration::from_millis(250)))?;
    let deadline = Instant::now() + timeout;
    let mut head = [0u8; 5];
    let mut got = 0;
    while got < head.len() {
        if live.cancelled() {
            return tell_cancel(&mut &*stream).map(|()| p::Accept::decode(&[])); // always an error
        }
        if Instant::now() > deadline {
            return Err(Error::NoAnswer);
        }
        match (&*stream).read(&mut head[got..]) {
            Ok(0) => return Err(Error::Io(io::ErrorKind::ConnectionAborted.into())),
            Ok(n) => got += n,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            Err(e) => return Err(e.into()),
        }
    }
    match Kind::try_from(head[0])? {
        Kind::Accept => {
            // The answer has arrived, so its few payload bytes follow right behind.
            let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
            if len > 64 {
                return Err(Error::protocol("oversized ACCEPT"));
            }
            stream.set_read_timeout(Some(timeout.min(Duration::from_secs(10))))?;
            let mut payload = vec![0u8; len];
            (&*stream).read_exact(&mut payload)?;
            Ok(p::Accept::decode(&payload))
        }
        Kind::Decline => Err(Error::Declined),
        _ => Err(Error::protocol("unexpected reply to offer")),
    }
}

/// Writes frames to the socket in order, then DONE.
///
/// On cancel, returns [`Error::Cancelled`] only if CANCEL actually reached the socket; any other
/// error means the receiver may not know the transfer is over.
pub(super) fn write_frames(
    stream: &TcpStream,
    frames: Receiver<Vec<u8>>,
    limit: Option<f64>,
    live: &Live,
    totals: Totals,
) -> Result<()> {
    let mut w = BufWriter::with_capacity(1 << 20, stream);
    let mut pace = limit.map(Pacer::new);
    loop {
        let f = match frames.recv_timeout(CANCEL_POLL) {
            Ok(f) => f,
            Err(RecvTimeoutError::Timeout) if !live.cancelled() => continue,
            Err(RecvTimeoutError::Timeout) => return tell_cancel(&mut w),
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if live.cancelled() {
            return tell_cancel(&mut w);
        }
        w.write_all(&f)?;
        live.wire_bytes.fetch_add(f.len() as u64, Relaxed);
        if let Some(pace) = pace.as_mut() {
            pace.sent(f.len(), live);
        }
    }
    w.write_all(&totals.encode(Kind::Done))?;
    w.flush()?;
    Ok(())
}

/// Tells the receiver the cancel is final, so it doesn't wait for a resume.
fn tell_cancel(w: &mut impl Write) -> Result<()> {
    w.write_all(&p::empty(Kind::Cancel))?;
    w.flush()?;
    Err(Error::Cancelled)
}

/// Reads ACKs until SUMMARY, which carries the receiver's final counts.
pub(super) fn read_acks(stream: &TcpStream, live: &Live) -> Result<Totals> {
    let mut r = BufReader::new(stream);
    loop {
        let (kind, payload) = p::read_frame(&mut r)?;
        match kind {
            Kind::Ack => {
                let t = Totals::decode(&payload)?;
                live.files_done.store(t.files, Relaxed);
                live.bytes_done.store(t.bytes, Relaxed);
            }
            Kind::Summary => return Totals::decode(&payload),
            Kind::Cancel => return Err(Error::CancelledByPeer),
            _ => return Err(Error::protocol("unexpected frame from receiver")),
        }
    }
}

/// Holds sending to an average rate, for testing slow links and for being polite on shared ones.
struct Pacer {
    rate: f64,
    start: Instant,
    sent: u64,
}

impl Pacer {
    fn new(bytes_per_sec: f64) -> Pacer {
        Pacer { rate: bytes_per_sec, start: Instant::now(), sent: 0 }
    }

    /// Sleeps until `n` more bytes would have fit through a link of this rate, or until cancelled.
    fn sent(&mut self, n: usize, live: &Live) {
        self.sent += n as u64;
        let due = Duration::from_secs_f64(self.sent as f64 / self.rate);
        // Windows sleeps round up to ~15 ms, so only sleep once well ahead; short bursts are fine.
        while let Some(left) = due.checked_sub(self.start.elapsed()).filter(|l| *l > Duration::from_millis(20)) {
            if live.cancelled() {
                return;
            }
            std::thread::sleep(left.min(CANCEL_POLL));
        }
    }
}
