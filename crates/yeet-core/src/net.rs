//! Socket setup shared by both sides.

use crate::{Error, Live, Result};
use std::io;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

/// A write blocked this long means the link is dead (or the peer badly stalled); either way the
/// transfer resumes on a new connection. Needed because on Windows, shutting a socket down from
/// another thread doesn't wake a blocked write.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long each connection attempt may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
/// After a cancel, how long to let a CANCEL frame get out before closing the socket.
const CANCEL_GRACE: Duration = Duration::from_secs(1);
/// Big enough to keep a ~1 Gbps link busy across Wi-Fi latency.
const SOCKET_BUFFER: usize = 4 << 20;

/// Keepalive so dead peers are noticed while idle, and bigger buffers for high-latency Wi-Fi.
pub(crate) fn tune_socket(s: &TcpStream) -> io::Result<()> {
    s.set_nodelay(true)?;
    s.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let sock = socket2::SockRef::from(s);
    let keepalive =
        socket2::TcpKeepalive::new().with_time(Duration::from_secs(10)).with_interval(Duration::from_secs(3));
    sock.set_tcp_keepalive(&keepalive)?;
    sock.set_send_buffer_size(SOCKET_BUFFER)?;
    sock.set_recv_buffer_size(SOCKET_BUFFER)?;
    Ok(())
}

/// The other end of `stream` is this computer (loopback, or one of its own addresses).
pub(crate) fn same_host(stream: &TcpStream) -> bool {
    match (stream.peer_addr(), stream.local_addr()) {
        (Ok(peer), Ok(local)) => peer.ip().is_loopback() || peer.ip() == local.ip(),
        _ => false,
    }
}

/// Tries every address at once and keeps the first that answers, so a dead Wi-Fi address
/// doesn't delay a working Ethernet one.
pub(crate) fn connect(addrs: &[SocketAddr], live: &Live) -> Result<TcpStream> {
    if live.cancelled() {
        return Err(Error::Cancelled);
    }
    if addrs.is_empty() {
        return Err(Error::Unreachable);
    }
    let (tx, rx) = crossbeam_channel::unbounded();
    for &addr in addrs {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT));
        });
    }
    drop(tx);
    let mut last = Error::Unreachable;
    for attempt in rx {
        match attempt {
            Ok(s) => return Ok(s),
            Err(e) => last = Error::Io(e),
        }
    }
    Err(last)
}

/// Runs `f` while a watcher closes `stream` shortly after the transfer is cancelled, so threads
/// blocked in a socket read or write wake up.
pub(crate) fn with_cancel_watch<T>(stream: &TcpStream, live: &Live, f: impl FnOnce() -> T) -> T {
    let done = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| {
            let mut cancelled_at = None;
            while !done.load(Relaxed) {
                if live.cancelled() && cancelled_at.get_or_insert_with(Instant::now).elapsed() > CANCEL_GRACE {
                    let _ = stream.shutdown(Shutdown::Both);
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let out = f();
        done.store(true, Relaxed);
        out
    })
}
