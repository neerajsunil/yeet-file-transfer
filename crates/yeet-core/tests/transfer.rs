//! End-to-end transfers over loopback: a real receiver, a real sender, real files.

use yeet_core::protocol::{self as p, Kind};
use yeet_core::recv::{self, Handler, Offer};
use yeet_core::send::{self, Compress, SendOpts};
use yeet_core::{Error, Live, Report, Result};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{fs, thread};

// ---------------------------------------------------------------- receiver side

enum Decision {
    Accept,
    Decline,
}

/// Records what the receiver was told, and answers offers without a person.
struct TestHandler {
    dest: PathBuf,
    decision: Decision,
    offers: AtomicUsize,
    live: Mutex<Option<Arc<Live>>>,
    done: crossbeam_channel::Sender<Finished>,
}

impl Handler for TestHandler {
    fn offer(&self, _: &Offer, _: &AtomicBool) -> Option<PathBuf> {
        self.offers.fetch_add(1, Relaxed);
        matches!(self.decision, Decision::Accept).then(|| self.dest.clone())
    }
    fn started(&self, _: &Offer, live: Arc<Live>) {
        *self.live.lock().unwrap() = Some(live);
    }
    fn finished(&self, _: &Offer, result: &Result<Report>, saved: &[PathBuf]) {
        let result = match result {
            Ok(report) => Ok(report.clone()),
            Err(e) => Err(Ended::from(e)),
        };
        let _ = self.done.send((result, saved.to_vec()));
    }
}

/// How the receiver's side ended, comparable in assertions.
#[derive(Debug, PartialEq)]
enum Ended {
    Cancelled,
    CancelledByPeer,
    Unsafe,
    Other(String),
}

impl From<&Error> for Ended {
    fn from(e: &Error) -> Ended {
        match e {
            Error::Cancelled => Ended::Cancelled,
            Error::CancelledByPeer => Ended::CancelledByPeer,
            Error::Unsafe(_) => Ended::Unsafe,
            other => Ended::Other(other.to_string()),
        }
    }
}

type Finished = (std::result::Result<Report, Ended>, Vec<PathBuf>);

struct Receiver {
    addr: SocketAddr,
    handler: &'static TestHandler,
    done: crossbeam_channel::Receiver<Finished>,
}

impl Receiver {
    fn start(dest: &Path, decision: Decision) -> Receiver {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, done) = crossbeam_channel::unbounded();
        let handler: &'static TestHandler = Box::leak(Box::new(TestHandler {
            dest: dest.to_path_buf(),
            decision,
            offers: AtomicUsize::new(0),
            live: Mutex::new(None),
            done: tx,
        }));
        thread::spawn(move || recv::serve(listener, 4, handler));
        Receiver { addr, handler, done }
    }

    fn outcome(&self) -> Finished {
        self.done.recv_timeout(Duration::from_secs(30)).expect("receiver never finished")
    }

    fn live(&self) -> Arc<Live> {
        let t = Instant::now();
        loop {
            if let Some(l) = self.handler.live.lock().unwrap().clone() {
                return l;
            }
            assert!(t.elapsed() < Duration::from_secs(10), "transfer never started");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

// ---------------------------------------------------------------- sender side

fn opts(sources: Vec<PathBuf>, to: SocketAddr, compress: Compress, limit: Option<f64>) -> SendOpts {
    SendOpts { sources, resolve: Arc::new(move || vec![to]), name: "test sender".into(), limit, compress, threads: 4, prepared: None, local: false }
}

/// A folder like a small game install: nested folders, many small files, a big compressible
/// file, a big incompressible one, and empty ones.
fn make_tree(root: &Path) -> PathBuf {
    let game = root.join("Game");
    fs::create_dir_all(game.join("data/maps")).unwrap();
    fs::create_dir_all(game.join("empty dir")).unwrap();
    for i in 0..300 {
        fs::write(game.join(format!("data/maps/m{i:03}.json")), format!("{{\"map\": {i}}}").repeat(i % 40 + 1))
            .unwrap();
    }
    fs::write(game.join("data/level.pak"), "level data ".repeat(800_000)).unwrap(); // ~8.8 MB, compressible
    let mut noise = vec![0u8; 3 << 20];
    fastrand_fill(&mut noise);
    fs::write(game.join("movie.bin"), noise).unwrap();
    fs::write(game.join("empty.txt"), "").unwrap();
    game
}

fn fastrand_fill(buf: &mut [u8]) {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for b in buf {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = x as u8;
    }
}

/// Every file and folder under `a` exists under `b` with the same contents, and nothing else.
fn assert_same_tree(a: &Path, b: &Path) {
    let list = |root: &Path| -> Vec<(PathBuf, Option<Vec<u8>>)> {
        let mut v: Vec<_> = walk(root)
            .into_iter()
            .map(|p| {
                let data = p.is_file().then(|| fs::read(&p).unwrap());
                (p.strip_prefix(root).unwrap().to_path_buf(), data)
            })
            .collect();
        v.sort();
        v
    };
    let (la, lb) = (list(a), list(b));
    assert_eq!(la.len(), lb.len(), "different number of entries");
    for (x, y) in la.iter().zip(&lb) {
        assert_eq!(x.0, y.0);
        assert!(x.1 == y.1, "contents differ: {}", x.0.display());
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        }
        out.push(p);
    }
    out
}

// ---------------------------------------------------------------- tests

#[test]
fn sends_a_folder_intact_and_compresses_what_shrinks() {
    let tmp = tempfile::tempdir().unwrap();
    let src = make_tree(&tmp.path().join("src"));
    let dest = tmp.path().join("dest");
    let rx = Receiver::start(&dest, Decision::Accept);

    let live = Live::default();
    let report = send::send(&opts(vec![src.clone()], rx.addr, Compress::Zstd(3), None), &live).unwrap();
    let (received, saved) = rx.outcome();

    assert_same_tree(&src, &dest.join("Game"));
    assert_eq!(saved, [dest.join("Game")]);
    assert_eq!(report.files, 303);
    assert_eq!(received.unwrap().bytes, report.bytes);
    assert!(report.wire < report.bytes * 2 / 3, "the .pak should compress: {} of {}", report.wire, report.bytes);
}

#[test]
fn sends_uncompressed_when_asked() {
    let tmp = tempfile::tempdir().unwrap();
    let src = make_tree(&tmp.path().join("src"));
    let dest = tmp.path().join("dest");
    let rx = Receiver::start(&dest, Decision::Accept);

    let report = send::send(&opts(vec![src.clone()], rx.addr, Compress::None, None), &Live::default()).unwrap();
    rx.outcome().0.unwrap();
    assert_same_tree(&src, &dest.join("Game"));
    assert!(report.wire >= report.bytes);
}

#[test]
fn auto_skips_compression_to_this_same_computer() {
    let tmp = tempfile::tempdir().unwrap();
    let src = make_tree(&tmp.path().join("src"));
    let dest = tmp.path().join("dest");
    let rx = Receiver::start(&dest, Decision::Accept);

    let report = send::send(&opts(vec![src.clone()], rx.addr, Compress::Auto(3), None), &Live::default()).unwrap();
    rx.outcome().0.unwrap();
    assert_same_tree(&src, &dest.join("Game"));
    assert!(report.wire >= report.bytes, "loopback has no wire to save");
}

#[test]
fn same_computer_copies_directly() {
    let tmp = tempfile::tempdir().unwrap();
    let src = make_tree(&tmp.path().join("src"));
    let dest = tmp.path().join("dest");
    let rx = Receiver::start(&dest, Decision::Accept);

    let mut o = opts(vec![src.clone()], rx.addr, Compress::Auto(3), None);
    o.local = true;
    let report = send::send(&o, &Live::default()).unwrap();
    let (received, _) = rx.outcome();
    assert_same_tree(&src, &dest.join("Game"));
    assert_eq!(received.unwrap().bytes, report.bytes);
    assert!(report.wire < report.bytes / 10, "only paths cross the socket: {} of {}", report.wire, report.bytes);
}

#[test]
fn uses_a_prepared_scan() {
    let tmp = tempfile::tempdir().unwrap();
    let src = make_tree(&tmp.path().join("src"));
    let dest = tmp.path().join("dest");
    let rx = Receiver::start(&dest, Decision::Accept);

    let mut o = opts(vec![src.clone()], rx.addr, Compress::None, None);
    o.prepared = Some(send::prepare(vec![src.clone()]));
    let report = send::send(&o, &Live::default()).unwrap();
    rx.outcome().0.unwrap();
    assert_same_tree(&src, &dest.join("Game"));
    assert_eq!(report.files, 303);
}

#[test]
fn existing_folders_are_not_overwritten() {
    let tmp = tempfile::tempdir().unwrap();
    let src = make_tree(&tmp.path().join("src"));
    let dest = tmp.path().join("dest");
    fs::create_dir_all(dest.join("Game")).unwrap();
    fs::write(dest.join("Game/mine.txt"), "keep me").unwrap();
    let rx = Receiver::start(&dest, Decision::Accept);

    send::send(&opts(vec![src.clone()], rx.addr, Compress::Auto(3), None), &Live::default()).unwrap();
    let (_, saved) = rx.outcome();
    assert_eq!(saved, [dest.join("Game (2)")]);
    assert_eq!(fs::read_to_string(dest.join("Game/mine.txt")).unwrap(), "keep me");
    assert_same_tree(&src, &dest.join("Game (2)"));
}

#[test]
fn declined_offers_write_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let src = make_tree(&tmp.path().join("src"));
    let dest = tmp.path().join("dest");
    let rx = Receiver::start(&dest, Decision::Decline);

    let err = send::send(&opts(vec![src], rx.addr, Compress::Auto(3), None), &Live::default()).unwrap_err();
    assert!(matches!(err, Error::Declined), "{err}");
    assert!(!dest.exists());
}

#[test]
fn receiver_cancel_stops_the_sender_and_removes_partial_files() {
    let tmp = tempfile::tempdir().unwrap();
    let src = make_tree(&tmp.path().join("src"));
    let dest = tmp.path().join("dest");
    let rx = Receiver::start(&dest, Decision::Accept);

    let src_root = src.parent().unwrap().to_path_buf();
    let sender = thread::spawn({
        let o = opts(vec![src], rx.addr, Compress::None, Some(4e6)); // ~3 s at this rate
        move || send::send(&o, &Live::default())
    });
    let live = rx.live();
    while live.bytes_done.load(Relaxed) == 0 {
        thread::sleep(Duration::from_millis(10));
    }
    live.request_cancel();

    let err = sender.join().unwrap().unwrap_err();
    assert!(matches!(err, Error::CancelledByPeer), "{err}");
    assert_eq!(rx.outcome().0, Err(Ended::Cancelled));
    // Files that finished before the cancel are kept; anything incomplete is removed.
    for f in walk(&dest).into_iter().filter(|p| p.is_file()) {
        let original = src_root.join(f.strip_prefix(&dest).unwrap());
        assert_eq!(
            f.metadata().unwrap().len(),
            original.metadata().unwrap().len(),
            "partial file left: {}",
            f.display()
        );
    }
}

#[test]
fn sender_cancel_is_reported_to_the_receiver() {
    let tmp = tempfile::tempdir().unwrap();
    let src = make_tree(&tmp.path().join("src"));
    let rx = Receiver::start(&tmp.path().join("dest"), Decision::Accept);

    let live = Arc::new(Live::default());
    let sender = thread::spawn({
        let (o, live) = (opts(vec![src], rx.addr, Compress::None, Some(4e6)), live.clone());
        move || send::send(&o, &live)
    });
    while live.bytes_done.load(Relaxed) == 0 && live.wire_bytes.load(Relaxed) < 1 << 20 {
        thread::sleep(Duration::from_millis(10));
    }
    live.request_cancel();
    assert!(matches!(sender.join().unwrap(), Err(Error::Cancelled)));
    assert_eq!(rx.outcome().0, Err(Ended::CancelledByPeer));
}

#[test]
fn resumes_after_the_connection_drops() {
    let tmp = tempfile::tempdir().unwrap();
    let src = make_tree(&tmp.path().join("src"));
    let dest = tmp.path().join("dest");
    let rx = Receiver::start(&dest, Decision::Accept);
    let proxy = Proxy::start(rx.addr);

    let sender = thread::spawn({
        let o = opts(vec![src.clone()], proxy.addr, Compress::None, Some(8e6));
        move || send::send(&o, &Live::default())
    });
    // Cut once data is on disk, not merely in flight: socket buffers lose what they hold.
    wait_for(|| rx.live().bytes_done.load(Relaxed) >= 2 << 20);
    proxy.cut.store(true, Relaxed);

    sender.join().unwrap().unwrap();
    rx.outcome().0.unwrap();
    assert_same_tree(&src, &dest.join("Game"));
    assert_eq!(rx.handler.offers.load(Relaxed), 1, "a resume must not ask the person again");
    let total: u64 = walk(&src).iter().filter(|p| p.is_file()).map(|p| p.metadata().unwrap().len()).sum();
    let resent = proxy.resent.load(Relaxed) as u64;
    assert!(
        resent > 0 && resent < total - (1 << 20),
        "resume sent {resent} of {total} bytes; should skip what arrived"
    );
}

#[test]
fn cancel_while_reconnecting_reaches_the_receiver() {
    let tmp = tempfile::tempdir().unwrap();
    let src = make_tree(&tmp.path().join("src"));
    let rx = Receiver::start(&tmp.path().join("dest"), Decision::Accept);
    let proxy = Proxy::start(rx.addr);

    let live = Arc::new(Live::default());
    let sender = thread::spawn({
        let (o, live) = (opts(vec![src], proxy.addr, Compress::None, Some(8e6)), live.clone());
        move || send::send(&o, &live)
    });
    wait_for(|| rx.live().bytes_done.load(Relaxed) > 0);
    proxy.refuse.store(true, Relaxed);
    proxy.cut.store(true, Relaxed);
    wait_for(|| live.reconnecting.load(Relaxed));
    // The network comes back just as the person gives up.
    proxy.refuse.store(false, Relaxed);
    live.request_cancel();

    assert!(matches!(sender.join().unwrap(), Err(Error::Cancelled)));
    assert_eq!(rx.outcome().0, Err(Ended::CancelledByPeer), "the receiver must not wait for a resume");
}

#[test]
fn refuses_paths_that_escape_the_destination() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("dest");
    let rx = Receiver::start(&dest, Decision::Accept);

    // A hand-rolled malicious sender.
    let mut s = TcpStream::connect(rx.addr).unwrap();
    let offer = p::Offer { transfer: 1, name: "evil".into(), files: 1, bytes: 4, items: vec!["x".into()] };
    s.write_all(p::MAGIC).unwrap();
    s.write_all(&offer.encode()).unwrap();
    let (kind, _) = p::read_frame(&mut s).unwrap();
    assert_eq!(kind, Kind::Accept);
    s.write_all(&p::file(0, 4, "../escaped.txt")).unwrap();

    assert_eq!(rx.outcome().0, Err(Ended::Unsafe));
    assert!(!tmp.path().join("escaped.txt").exists());
}

/// A TCP proxy between sender and receiver that simulates network trouble.
struct Proxy {
    addr: SocketAddr,
    /// Breaks the first connection at its next read, like a pulled cable.
    cut: Arc<AtomicBool>,
    /// While set, new connections are closed straight away.
    refuse: Arc<AtomicBool>,
    /// Bytes from the sender on connections after the first.
    resent: Arc<AtomicUsize>,
}

impl Proxy {
    fn start(target: SocketAddr) -> Proxy {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy = Proxy {
            addr: listener.local_addr().unwrap(),
            cut: Arc::default(),
            refuse: Arc::default(),
            resent: Arc::default(),
        };
        let (cut, refuse, resent) = (proxy.cut.clone(), proxy.refuse.clone(), proxy.resent.clone());
        thread::spawn(move || {
            for (n, conn) in listener.incoming().enumerate() {
                let Ok(client) = conn else { continue };
                if refuse.load(Relaxed) {
                    continue; // dropping `client` closes it
                }
                let server = TcpStream::connect(target).unwrap();
                let (cut, count) = if n == 0 { (Some(cut.clone()), None) } else { (None, Some(resent.clone())) };
                pipe(client.try_clone().unwrap(), server.try_clone().unwrap(), cut, count);
                pipe(server, client, None, None);
            }
        });
        proxy
    }
}

fn pipe(mut from: TcpStream, mut to: TcpStream, cut: Option<Arc<AtomicBool>>, count: Option<Arc<AtomicUsize>>) {
    thread::spawn(move || {
        let mut buf = vec![0u8; 64 << 10];
        while let Ok(n) = from.read(&mut buf) {
            if n == 0 || cut.as_ref().is_some_and(|c| c.load(Relaxed)) || to.write_all(&buf[..n]).is_err() {
                break;
            }
            if let Some(c) = &count {
                c.fetch_add(n, Relaxed);
            }
        }
        let _ = from.shutdown(std::net::Shutdown::Both);
        let _ = to.shutdown(std::net::Shutdown::Both);
    });
}

fn wait_for(condition: impl Fn() -> bool) {
    let t = Instant::now();
    while !condition() {
        assert!(t.elapsed() < Duration::from_secs(20), "timed out waiting");
        thread::sleep(Duration::from_millis(5));
    }
}
