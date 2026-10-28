//! Accepted transfers, kept alive across reconnects.

use super::files::Files;
use super::paths::Placer;
use super::Offer;
use crate::disk::Disk;
use crate::{Live, Result};
use std::collections::HashMap;
use std::net::{Shutdown, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(super) struct Session {
    pub offer: Offer,
    pub live: Arc<Live>,
    pub started: Instant,
    /// The connection currently serving this session, so a reconnect can push out a stale one.
    conn: Mutex<Option<TcpStream>>,
    /// Set while no connection is serving it; used to expire abandoned sessions.
    idle_since: Mutex<Option<Instant>>,
    /// Held by whichever connection is serving the session.
    pub state: Mutex<State>,
}

/// Everything about a session's files that must survive a reconnect.
pub(super) struct State {
    pub placer: Placer,
    pub files: Files,
    /// The drive being saved to.
    pub disk: Disk,
}

impl Session {
    /// Records `stream` as the connection serving this session.
    pub fn attach(&self, stream: &TcpStream) {
        *self.conn.lock().expect("poisoned") = stream.try_clone().ok();
        *self.idle_since.lock().expect("poisoned") = None;
    }

    /// The connection ended; the session waits to be resumed or expired.
    pub fn detach(&self) {
        *self.conn.lock().expect("poisoned") = None;
        *self.idle_since.lock().expect("poisoned") = Some(Instant::now());
    }

    /// Closes the serving connection, if any, so its thread lets go of `state`.
    pub fn drop_connection(&self) {
        if let Some(old) = self.conn.lock().expect("poisoned").as_ref() {
            let _ = old.shutdown(Shutdown::Both);
        }
    }

    fn expired(&self, window: Duration) -> bool {
        self.idle_since.lock().expect("poisoned").is_some_and(|t| t.elapsed() > window || self.live.cancelled())
    }
}

#[derive(Default)]
pub(super) struct Sessions(Mutex<HashMap<u64, Arc<Session>>>);

impl Sessions {
    /// A session this offer continues: same transfer id, from the same device name.
    pub fn find_resumable(&self, offer: &Offer) -> Option<Arc<Session>> {
        let map = self.0.lock().expect("poisoned");
        map.get(&offer.transfer).filter(|s| s.offer.name == offer.name).cloned()
    }

    /// Starts a session for an accepted offer, saving into `dest`.
    pub fn start(&self, offer: Offer, dest: PathBuf) -> Result<Arc<Session>> {
        std::fs::create_dir_all(&dest)?;
        let disk = Disk::probe(&dest);
        let live = Arc::new(Live::default());
        live.files_total.store(offer.files, Relaxed);
        live.bytes_total.store(offer.bytes, Relaxed);
        live.accepted.store(true, Relaxed);
        let s = Arc::new(Session {
            offer,
            live,
            started: Instant::now(),
            conn: Mutex::new(None),
            idle_since: Mutex::new(None),
            state: Mutex::new(State { placer: Placer::new(dest), files: Files::new(disk.preallocate()), disk }),
        });
        self.0.lock().expect("poisoned").insert(s.offer.transfer, s.clone());
        Ok(s)
    }

    pub fn remove(&self, transfer: u64) {
        self.0.lock().expect("poisoned").remove(&transfer);
    }

    /// Removes and returns sessions idle longer than `window`, or cancelled while idle.
    pub fn take_expired(&self, window: Duration) -> Vec<Arc<Session>> {
        let mut map = self.0.lock().expect("poisoned");
        let ids: Vec<u64> = map.iter().filter(|(_, s)| s.expired(window)).map(|(id, _)| *id).collect();
        ids.iter().filter_map(|id| map.remove(id)).collect()
    }
}
