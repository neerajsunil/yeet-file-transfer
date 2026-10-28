//! What the buttons do. Each action changes [`State`](crate::state::State) and, for slow work,
//! starts a background thread; the window catches up on its next refresh.

use crate::platform;
use crate::settings::DEFAULT_PORT;
use crate::state::{App, Device, Item, State, Transfer};
use crate::text;
use yeet_core::discovery::Peer;
use yeet_core::send::{self, Compress, SendOpts};
use yeet_core::{default_threads, Live};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;

/// zstd level used when compression is on: fast enough to keep up with gigabit.
const ZSTD_LEVEL: i32 = 3;

/// Adds picked files and folders to the send list, measuring folders in the background.
pub fn add_items(app: &Arc<App>, paths: Vec<PathBuf>) {
    let mut st = app.state();
    for path in paths {
        if st.items.iter().any(|i| i.path == path) {
            continue;
        }
        let folder = path.is_dir();
        let size = if folder {
            measure_in_background(app, path.clone());
            None
        } else {
            Some(std::fs::metadata(&path).map_or(0, |m| m.len()))
        };
        st.items.push(Item { path, folder, size });
    }
    reprepare(&mut st);
}

/// Starts getting the chosen items ready to send (scan, drive probe, read-ahead), so that by
/// the time a device is picked and Send pressed, most of the waiting is already done.
pub fn reprepare(st: &mut State) {
    let sources: Vec<PathBuf> = st.items.iter().map(|i| i.path.clone()).collect();
    st.prepared = (!sources.is_empty()).then(|| send::prepare(sources));
}

fn measure_in_background(app: &Arc<App>, path: PathBuf) {
    let app = app.clone();
    std::thread::spawn(move || {
        let total = walkdir::WalkDir::new(&path)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum();
        if let Some(item) = app.state().items.iter_mut().find(|i| i.path == path) {
            item.size = Some(total);
        }
    });
}

/// Sends the chosen items to the selected device.
pub fn start_send(app: &Arc<App>) {
    let mut st = app.state();
    let Some(device) = st.selected.as_ref().and_then(|id| st.devices.iter().find(|d| &d.peer.id == id)) else {
        return;
    };
    if st.items.is_empty() {
        return;
    }
    let peer = device.peer.clone();
    let sources: Vec<PathBuf> = st.items.drain(..).map(|i| i.path).collect();
    let names: Vec<String> = sources.iter().map(|p| text::file_name(p)).collect();
    let live = Arc::new(Live::default());
    let key = fastrand::u64(..);
    st.transfers.insert(0, Transfer::new(key, false, peer.name.clone(), text::what(&names), live.clone()));
    let opts = SendOpts {
        sources,
        // Look the device up again on every (re)connect: its address may have changed.
        resolve: {
            let (app, id, fallback) = (app.clone(), peer.id.clone(), peer.addrs.clone());
            Arc::new(move || app.addrs_for(&id).unwrap_or_else(|| fallback.clone()))
        },
        name: st.settings.device_name.clone(),
        limit: None,
        compress: if st.settings.compression { Compress::Auto(ZSTD_LEVEL) } else { Compress::None },
        threads: default_threads(),
        prepared: st.prepared.take(),
        local: true,
    };
    drop(st);
    let app = app.clone();
    std::thread::spawn(move || {
        let result = send::send(&opts, &live);
        let mut st = app.state();
        if let Some(t) = st.transfers.iter_mut().find(|t| !t.incoming && t.key == key) {
            t.outcome = Some(result.map_err(|e| text::describe(&e, &peer.name, false)));
        }
    });
}

/// Copies the chosen items into `dest` on this computer ("This PC"). Runs a private, one-shot
/// receiver on loopback and sends to it, so the copy gets the same progress, cancel and
/// renaming as any transfer, while the engine's same-computer path does the actual copying.
pub fn copy_here(app: &Arc<App>, dest: PathBuf) {
    let listener = match std::net::TcpListener::bind(("127.0.0.1", 0)) {
        Ok(l) => l,
        Err(e) => return app.toast(format!("Couldn't start the copy: {e}")),
    };
    let Ok(addr) = listener.local_addr() else { return };
    let mut st = app.state();
    if st.items.is_empty() {
        return;
    }
    let sources: Vec<PathBuf> = st.items.drain(..).map(|i| i.path).collect();
    let names: Vec<String> = sources.iter().map(|p| text::file_name(p)).collect();
    let live = Arc::new(Live::default());
    let key = fastrand::u64(..);
    st.transfers.insert(0, Transfer::new(key, false, THIS_PC.into(), text::what(&names), live.clone()));
    let opts = SendOpts {
        sources,
        resolve: Arc::new(move || vec![addr]),
        name: THIS_PC.into(),
        limit: None,
        compress: Compress::None,
        threads: default_threads(),
        prepared: st.prepared.take(),
        local: true,
    };
    drop(st);
    yeet_core::disk::prewarm(vec![dest.clone()]);

    let handler = CopyHere { app: app.clone(), key, dest };
    std::thread::spawn(move || {
        if let Ok((stream, peer)) = listener.accept() {
            let _ = yeet_core::recv::handle(stream, peer, yeet_core::RECEIVE_WRITERS, &handler);
        }
    });
    let app = app.clone();
    std::thread::spawn(move || {
        let result = send::send(&opts, &live);
        let mut st = app.state();
        if let Some(t) = st.transfers.iter_mut().find(|t| !t.incoming && t.key == key) {
            t.outcome = Some(result.map_err(|e| text::describe(&e, THIS_PC, false)));
        }
    });
}

const THIS_PC: &str = "This PC";

/// The receiving half of [`copy_here`]: no one to ask, and the sending half already shows the
/// transfer, so it only records where things were saved.
struct CopyHere {
    app: Arc<App>,
    key: u64,
    dest: PathBuf,
}

impl yeet_core::recv::Handler for CopyHere {
    fn offer(&self, _: &yeet_core::recv::Offer, _: &std::sync::atomic::AtomicBool) -> Option<PathBuf> {
        Some(self.dest.clone())
    }

    fn started(&self, _: &yeet_core::recv::Offer, _: Arc<Live>) {}

    fn finished(&self, _: &yeet_core::recv::Offer, _: &yeet_core::Result<yeet_core::Report>, saved: &[PathBuf]) {
        if let Some(t) = self.app.state().transfers.iter_mut().find(|t| !t.incoming && t.key == self.key) {
            t.saved = saved.to_vec();
        }
    }
}

/// Answers the incoming offer being shown, if any.
pub fn answer_offer(app: &App, accept: bool) {
    if let Some(offer) = app.state().offer.take() {
        let _ = offer.reply.send(accept);
    }
}

/// Asks Windows Security (through an admin prompt) to stop scanning the save folder.
pub fn exclude_defender(app: &Arc<App>) {
    let (dir, defender, short_names) = {
        let mut st = app.state();
        if st.defender_busy {
            return;
        }
        st.defender_busy = true;
        let dir = st.settings.save_dir.clone();
        let defender = st.defender && !st.settings.defender_excluded.contains(&dir);
        (dir, defender, st.short_names)
    };
    let app = app.clone();
    std::thread::spawn(move || {
        let ok = platform::defender_exclude(&dir, defender, short_names);
        let message: String = {
            let mut st = app.state();
            st.defender_busy = false;
            if ok {
                if defender {
                    st.settings.defender_excluded.push(dir.clone());
                    st.settings.save();
                }
                st.short_names = false;
                "Done. Receiving into this folder is faster now.".into()
            } else {
                "Nothing changed. Windows needs administrator approval for this.".into()
            }
        };
        app.toast(message);
    });
}

/// Adds a device by IP address or host name, for networks where discovery is blocked.
/// Returns false (after telling the person) if the address doesn't resolve.
pub fn connect_manual(app: &App, text: &str) -> bool {
    let text = text.trim();
    let target =
        if text.contains(':') && !text.contains("::") { text.to_string() } else { format!("{text}:{DEFAULT_PORT}") };
    let addrs: Vec<SocketAddr> = target.to_socket_addrs().map(Iterator::collect).unwrap_or_default();
    if addrs.is_empty() {
        app.toast(format!("“{text}” isn't a valid address."));
        return false;
    }
    let id = format!("manual:{target}");
    let mut st = app.state();
    if !st.devices.iter().any(|d| d.peer.id == id) {
        st.devices.push(Device { peer: Peer { id: id.clone(), name: text.to_string(), addrs }, manual: true });
    }
    st.selected = Some(id);
    true
}

/// How other devices can reach this one, for the settings sheet.
pub fn own_address(app: &App) -> String {
    match platform::local_address() {
        Some(ip) if app.port == DEFAULT_PORT => ip.to_string(),
        Some(ip) => format!("{ip}:{}", app.port),
        None => "unknown".into(),
    }
}
