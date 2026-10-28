//! Everything the app knows, shared between the UI thread and background threads.
//!
//! Background threads (discovery, receiving, sending) only ever change [`State`]; the UI thread
//! mirrors it into the window on a timer (see `view`). Nothing outside the UI thread touches
//! Slint objects.

use crate::settings::Settings;
use yeet_core::discovery::{Discovery, Peer};
use yeet_core::{Live, Report};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub struct App {
    state: Mutex<State>,
    pub discovery: Mutex<Option<Discovery>>,
    /// The port actually listened on (the configured one may have been taken).
    pub port: u16,
}

pub struct State {
    pub settings: Settings,
    /// Chosen for sending.
    pub items: Vec<Item>,
    /// A head start on sending `items`: scanned and partly read while a device is being chosen.
    pub prepared: Option<yeet_core::send::Prepared>,
    pub devices: Vec<Device>,
    /// Id of the device to send to.
    pub selected: Option<String>,
    /// Newest first.
    pub transfers: Vec<Transfer>,
    /// An incoming offer waiting for the person to answer.
    pub offer: Option<PendingOffer>,
    pub toast: Option<(String, Instant)>,
    /// Defender's real-time scanning is on.
    pub defender: bool,
    /// The save folder's volume makes 8.3 short names.
    pub short_names: bool,
    /// Waiting on the admin prompt for a Defender exclusion.
    pub defender_busy: bool,
}

pub struct Item {
    pub path: PathBuf,
    pub folder: bool,
    /// Total bytes; `None` while a folder is still being measured.
    pub size: Option<u64>,
}

pub struct Device {
    pub peer: Peer,
    /// Added by IP address rather than discovered.
    pub manual: bool,
}

pub struct Transfer {
    /// Incoming: the sender's transfer id. Outgoing: random.
    pub key: u64,
    pub incoming: bool,
    pub peer: String,
    /// What's being sent, e.g. "Photos" or "3 items".
    pub what: String,
    pub live: Arc<Live>,
    /// `None` while running; the error is already worded for people.
    pub outcome: Option<Result<Report, String>>,
    /// Top-level paths created by an incoming transfer.
    pub saved: Vec<PathBuf>,
    pub meter: Meter,
}

impl Transfer {
    pub fn new(key: u64, incoming: bool, peer: String, what: String, live: Arc<Live>) -> Transfer {
        Transfer { key, incoming, peer, what, live, outcome: None, saved: Vec::new(), meter: Meter::default() }
    }

    pub fn running(&self) -> bool {
        self.outcome.is_none()
    }
}

pub struct PendingOffer {
    pub from: String,
    pub summary: String,
    pub items: Vec<String>,
    pub reply: crossbeam_channel::Sender<bool>,
}

/// Speed and time left for a running transfer.
///
/// Time left comes from the average speed over the whole run, so it drifts with real slowdowns
/// instead of jumping around, and the shown value counts down and eases toward that estimate.
/// Time spent waiting, reconnecting or stalled isn't counted, so a hiccup never sends it to
/// infinity: the estimate freezes and picks up where it left off.
#[derive(Default)]
pub struct Meter {
    last: Option<(Instant, u64)>,
    /// Counted time since bytes last arrived. Added to the average only once they arrive again,
    /// and then at most [`Self::STALL`] of it, so a stall doesn't drag the average down.
    idle: Duration,
    /// Time and bytes that count toward the average.
    active: Duration,
    moved: u64,
    /// Short-term speeds, only for display: file data, and bytes on the network.
    recent: f64,
    link: f64,
    last_wire: u64,
    /// Time left at the average speed, as of the last bytes.
    target: Option<f64>,
    /// Time left as shown.
    left: Option<f64>,
}

pub struct Reading {
    /// Recent file data per second; 0 until known.
    pub speed: f64,
    /// Recent bytes on the network per second, after compression; 0 when not tracked.
    pub link: f64,
    /// Seconds left, once the average has settled.
    pub left: Option<f64>,
    /// No bytes for a while.
    pub stalled: bool,
}

impl Meter {
    /// No bytes for this long counts as a stall.
    const STALL: Duration = Duration::from_secs(5);
    /// Don't estimate time left until the average has this much to go on.
    const SETTLE: Duration = Duration::from_secs(3);
    /// How quickly the shown speed follows the current one.
    const SPEED_TAU: f64 = 3.0;
    /// How quickly the shown time left follows the estimate.
    const LEFT_TAU: f64 = 6.0;

    /// Records progress. `counting` is false while waiting to be accepted or reconnecting.
    /// `wire` is bytes put on the network so far, or 0 when not tracked.
    pub fn sample(&mut self, done: u64, wire: u64, total: u64, counting: bool) -> Reading {
        self.sample_at(Instant::now(), done, wire, total, counting)
    }

    fn sample_at(&mut self, now: Instant, done: u64, wire: u64, total: u64, counting: bool) -> Reading {
        let wire_delta = wire.saturating_sub(std::mem::replace(&mut self.last_wire, wire));
        let Some((then, before)) = self.last.replace((now, done)) else {
            return self.reading(false);
        };
        let dt = now.duration_since(then);
        // A resume can rewind the count; that isn't progress.
        let delta = done.saturating_sub(before);
        if !counting {
            self.idle = Duration::ZERO;
            return self.reading(false);
        }
        self.idle += dt;
        let stalled = delta == 0 && self.idle > Self::STALL;
        if stalled || dt.is_zero() {
            return self.reading(stalled);
        }

        let secs = dt.as_secs_f64();
        let ease = |shown: f64, current: f64| {
            if shown == 0.0 {
                current
            } else {
                shown + (current - shown) * (1.0 - (-secs / Self::SPEED_TAU).exp())
            }
        };
        self.recent = ease(self.recent, delta as f64 / secs);
        if wire > 0 {
            self.link = ease(self.link, wire_delta as f64 / secs);
        }

        if delta > 0 {
            self.active += self.idle.min(Self::STALL);
            self.moved += delta;
            self.idle = Duration::ZERO;
            if self.active >= Self::SETTLE {
                let average = self.moved as f64 / self.active.as_secs_f64();
                self.target = Some(total.saturating_sub(done) as f64 / average);
            }
        }
        if let Some(target) = self.target {
            self.left = Some(match self.left {
                None => target,
                Some(left) => {
                    let left = (left - secs).max(0.0);
                    left + (target - left) * (1.0 - (-secs / Self::LEFT_TAU).exp())
                }
            });
        }
        self.reading(false)
    }

    fn reading(&self, stalled: bool) -> Reading {
        Reading { speed: self.recent, link: self.link, left: self.left, stalled }
    }
}

impl App {
    pub fn new(settings: Settings, port: u16) -> App {
        App {
            state: Mutex::new(State {
                settings,
                items: Vec::new(),
                prepared: None,
                devices: Vec::new(),
                selected: None,
                transfers: Vec::new(),
                offer: None,
                toast: None,
                defender: false,
                short_names: false,
                defender_busy: false,
            }),
            discovery: Mutex::new(None),
            port,
        }
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("app state poisoned")
    }

    /// Shows a short message at the bottom of the window.
    pub fn toast(&self, msg: impl Into<String>) {
        self.state().toast = Some((msg.into(), Instant::now()));
    }

    /// A device's current addresses, which change when it moves networks.
    pub fn addrs_for(&self, id: &str) -> Option<Vec<SocketAddr>> {
        self.state().devices.iter().find(|d| d.peer.id == id).map(|d| d.peer.addrs.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1_000_000;

    /// Feeds `secs` of ticks at `rate` bytes/s, returning the last reading and bytes done.
    fn run(m: &mut Meter, t: &mut Instant, done: &mut u64, total: u64, rate: u64, secs: u64) -> Reading {
        let mut r = m.sample_at(*t, *done, 0, total, true);
        for _ in 0..secs * 10 {
            *t += Duration::from_millis(100);
            *done += rate / 10;
            r = m.sample_at(*t, *done, 0, total, true);
        }
        r
    }

    #[test]
    fn steady_speed_gives_accurate_time_left() {
        let (mut m, mut t, mut done) = (Meter::default(), Instant::now(), 0);
        let r = run(&mut m, &mut t, &mut done, 100 * MB, MB, 10);
        assert!((r.left.unwrap() - 90.0).abs() < 1.0, "{:?}", r.left);
        assert!((r.speed - MB as f64).abs() < 1e3);
    }

    #[test]
    fn slowdown_moves_time_left_gradually() {
        let (mut m, mut t, mut done) = (Meter::default(), Instant::now(), 0);
        let before = run(&mut m, &mut t, &mut done, 1000 * MB, 10 * MB, 20).left.unwrap();
        let after = run(&mut m, &mut t, &mut done, 1000 * MB, MB, 1).left.unwrap();
        // A tenfold slowdown for a second nudges the estimate, it doesn't jump tenfold.
        assert!(after < before * 1.5, "{before} -> {after}");
    }

    #[test]
    fn stall_freezes_estimate_instead_of_growing() {
        let (mut m, mut t, mut done) = (Meter::default(), Instant::now(), 0);
        let before = run(&mut m, &mut t, &mut done, 100 * MB, MB, 10).left.unwrap();
        let r = run(&mut m, &mut t, &mut done, 100 * MB, 0, 60);
        assert!(r.stalled);
        let left = r.left.unwrap();
        assert!(left.is_finite() && left <= before + 5.0, "{before} -> {left}");
    }

    #[test]
    fn link_speed_tracks_wire_bytes() {
        let (mut m, mut t) = (Meter::default(), Instant::now());
        m.sample_at(t, 0, 1, 100 * MB, true);
        let mut r = None;
        for i in 1..=50 {
            t += Duration::from_millis(100);
            r = Some(m.sample_at(t, i * MB / 5, 1 + i * MB / 10, 100 * MB, true));
        }
        let r = r.unwrap();
        assert!((r.speed - 2e6).abs() < 1e3 && (r.link - 1e6).abs() < 1e3, "{} {}", r.speed, r.link);
    }

    #[test]
    fn reconnecting_time_isnt_counted() {
        let (mut m, mut t, mut done) = (Meter::default(), Instant::now(), 0);
        run(&mut m, &mut t, &mut done, 100 * MB, MB, 10);
        for _ in 0..300 {
            t += Duration::from_millis(100);
            m.sample_at(t, done, 0, 100 * MB, false);
        }
        let r = run(&mut m, &mut t, &mut done, 100 * MB, MB, 1);
        assert!(!r.stalled);
        assert!((r.left.unwrap() - 89.0).abs() < 2.0, "{:?}", r.left);
    }
}
