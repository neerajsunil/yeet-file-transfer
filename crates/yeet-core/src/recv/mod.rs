//! Receiving side.
//!
//! ```text
//! reader (1 thread)    read the offer, ask the Handler, then parse frames: create folders,
//!                      register files, hand chunks on
//!   -> writers (N)     decompress and write each segment, in parallel across files
//! acker (1 thread)     send progress ACKs, which double as a heartbeat
//! ```
//!
//! Files are created by the writers, so many small files are created concurrently. Each file is
//! written strictly front to back (see [`files`]), so "bytes written" is always a clean prefix,
//! which is what makes resuming exact.
//!
//! Every accepted transfer is a [`session::Session`]. If the connection drops, the session waits
//! for the sender to reconnect with the same transfer id and continues where it stopped.

mod copy;
mod files;
mod paths;
mod pipeline;
mod session;

use crate::net::{same_host, tune_socket, with_cancel_watch};
use crate::protocol::{self as p, Kind};
use crate::{Error, Live, Report, Result};
use session::{Session, Sessions};
use std::io::{BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;

/// How long an interrupted transfer waits for the sender to come back.
const RESUME_WINDOW: Duration = Duration::from_secs(600);
/// No data from the sender for this long means the connection is dead.
const READ_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a new connection may take to say what it wants.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// How often to look for abandoned sessions.
const EXPIRE_EVERY: Duration = Duration::from_secs(5);

/// What a sender wants to send, shown to the person before anything is written.
#[derive(Clone, Debug)]
pub struct Offer {
    pub peer: SocketAddr,
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
    fn new(peer: SocketAddr, o: p::Offer) -> Offer {
        Offer { peer, transfer: o.transfer, name: o.name, files: o.files, bytes: o.bytes, items: o.items }
    }
}

/// The receiving side's decisions and notifications. Called from connection threads.
pub trait Handler: Sync {
    /// Returns the folder to save into, or `None` to decline. May block while a person decides;
    /// `withdrawn` turns true if the sender gives up meanwhile.
    fn offer(&self, offer: &Offer, withdrawn: &AtomicBool) -> Option<PathBuf>;
    /// A transfer was accepted and is starting. `live` stays the same across reconnects.
    fn started(&self, offer: &Offer, live: Arc<Live>);
    /// The transfer is over for good. `saved` lists the top-level files and folders created,
    /// after any renaming to avoid existing ones.
    fn finished(&self, offer: &Offer, result: &Result<Report>, saved: &[PathBuf]);
}

/// Accepts connections forever. Each connection runs on its own thread, so a sender can
/// reconnect while its stale connection is still timing out, and several senders can send at once.
pub fn serve(listener: TcpListener, threads: usize, handler: &dyn Handler) -> Result<()> {
    let sessions = Sessions::default();
    std::thread::scope(|s| {
        s.spawn(|| loop {
            std::thread::sleep(EXPIRE_EVERY);
            expire(&sessions, handler);
        });
        loop {
            // Accept errors (e.g. a reset before accept) only affect that one connection.
            let Ok((stream, peer)) = listener.accept() else { continue };
            let sessions = &sessions;
            s.spawn(move || {
                // A bad or hostile sender must never take the listener down. Senders race
                // connections over every address and drop the losers before they say anything;
                // those end in EOF and aren't worth reporting.
                match handle_conn(stream, peer, threads, handler, sessions) {
                    Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
                    Err(e) => eprintln!("connection from {peer}: {e}"),
                    Ok(()) => {}
                }
            });
        }
    })
}

/// Handles a single connection without session reuse, for one-shot receiving.
pub fn handle(stream: TcpStream, peer: SocketAddr, threads: usize, handler: &dyn Handler) -> Result<()> {
    handle_conn(stream, peer, threads, handler, &Sessions::default())
}

fn handle_conn(
    stream: TcpStream,
    peer: SocketAddr,
    threads: usize,
    handler: &dyn Handler,
    sessions: &Sessions,
) -> Result<()> {
    tune_socket(&stream)?;
    stream.set_read_timeout(Some(HELLO_TIMEOUT))?;
    let mut r = BufReader::with_capacity(1 << 20, &stream);
    let offer = read_hello(&mut r, peer)?;

    let session = match sessions.find_resumable(&offer) {
        // Same transfer reconnecting: no need to ask again; push out the stale connection.
        Some(s) => {
            s.drop_connection();
            s
        }
        None => match ask(&stream, &offer, handler)? {
            Some(dest) => {
                let s = sessions.start(offer, dest)?;
                handler.started(&s.offer, s.live.clone());
                s
            }
            None => {
                let _ = (&stream).write_all(&p::empty(Kind::Decline));
                return Ok(());
            }
        },
    };

    // Waits here until a stale connection for this session has let go.
    let mut state = session.state.lock().expect("session state poisoned");
    session.attach(&stream);
    let live = &session.live;
    if live.cancelled() {
        drop(state);
        return finish(sessions, &session, Err(Error::Cancelled), handler);
    }
    live.reconnecting.store(false, Relaxed);
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    // Copying straight from the sender's paths is only safe when the sender is this computer.
    let local = same_host(&stream);
    (&stream).write_all(&p::Accept { disk: state.disk, local }.encode())?;
    let have = state.files.prepare_resume();
    live.files_done.store(have.iter().filter(|h| h.complete).count() as u64, Relaxed);
    live.bytes_done.store(have.iter().map(|h| h.written).sum(), Relaxed);
    let have: Vec<(u64, u64)> = have.iter().map(|h| (h.id, h.written)).collect();
    (&stream).write_all(&p::have(&have))?;

    let threads = crate::disk::writers(&state.disk, threads);
    let result = with_cancel_watch(&stream, live, || pipeline::run(&stream, &mut r, &mut state, threads, local, live))
        .map_err(|e| if live.cancelled() { Error::Cancelled } else { e });
    // Update the session while still holding `state`, so a reconnect waiting on it sees this first.
    session.detach();
    if matches!(&result, Err(e) if e.is_disconnect()) {
        // Keep everything and wait for the sender to come back.
        live.reconnecting.store(true, Relaxed);
        return Ok(());
    }
    drop(state);
    finish(sessions, &session, result, handler)
}

/// Reads the magic and the offer.
fn read_hello(r: &mut impl Read, peer: SocketAddr) -> Result<Offer> {
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if &magic != p::MAGIC {
        return Err(Error::protocol("not a yeet sender, or a different version"));
    }
    match p::read_frame(r)? {
        (Kind::Offer, payload) => Ok(Offer::new(peer, p::Offer::decode(&payload)?)),
        _ => Err(Error::protocol("expected an offer")),
    }
}

/// Asks the handler, watching for the sender giving up meanwhile.
fn ask(stream: &TcpStream, offer: &Offer, handler: &dyn Handler) -> Result<Option<PathBuf>> {
    let withdrawn = AtomicBool::new(false);
    let stop = AtomicBool::new(false);
    let dest = std::thread::scope(|s| {
        s.spawn(|| watch_withdraw(stream, &withdrawn, &stop));
        let d = handler.offer(offer, &withdrawn);
        stop.store(true, Relaxed);
        d
    });
    Ok(dest.filter(|_| !withdrawn.load(Relaxed)))
}

/// While the person decides, the sender sends nothing; anything arriving (CANCEL or EOF)
/// means it gave up.
fn watch_withdraw(stream: &TcpStream, withdrawn: &AtomicBool, stop: &AtomicBool) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let mut b = [0u8; 1];
    while !stop.load(Relaxed) {
        match stream.peek(&mut b) {
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            _ => {
                withdrawn.store(true, Relaxed);
                return;
            }
        }
    }
}

/// Ends a session for good: success, cancel, or an error that resuming can't fix.
fn finish(sessions: &Sessions, session: &Arc<Session>, result: Result<Report>, handler: &dyn Handler) -> Result<()> {
    sessions.remove(session.offer.transfer);
    session.live.reconnecting.store(false, Relaxed);
    let state = session.state.lock().expect("session state poisoned");
    let result = result.map(|r| Report { secs: session.started.elapsed().as_secs_f64(), ..r });
    if result.is_err() {
        state.files.remove_partials();
    }
    handler.finished(&session.offer, &result, state.placer.saved());
    result.map(|_| ())
}

/// Gives up on sessions whose sender hasn't come back in time, or that were cancelled here
/// while waiting.
fn expire(sessions: &Sessions, handler: &dyn Handler) {
    for s in sessions.take_expired(RESUME_WINDOW) {
        let state = s.state.lock().expect("session state poisoned");
        state.files.remove_partials();
        let err = if s.live.cancelled() { Error::Cancelled } else { Error::PeerDidNotReturn };
        handler.finished(&s.offer, &Err(err), state.placer.saved());
    }
}
