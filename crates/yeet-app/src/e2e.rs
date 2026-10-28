//! Hooks for headless end-to-end tests of two real app instances. Only built with
//! `--features e2e`, so release builds never act on these environment variables.
//!
//! - `YEET_AUTO_ACCEPT=1`: accept every offer without asking.
//! - `YEET_TEST_SEND=<path>`: send `path` to the first device found, print the outcome and
//!   exit (0 on success).

use crate::actions;
use crate::state::App;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const FIND_DEVICE_WITHIN: Duration = Duration::from_secs(30);

pub fn auto_accept() -> bool {
    std::env::var_os("YEET_AUTO_ACCEPT").is_some()
}

pub fn start(app: &Arc<App>) {
    if let Some(path) = std::env::var_os("YEET_TEST_SEND") {
        let app = app.clone();
        std::thread::spawn(move || send_and_exit(&app, PathBuf::from(path)));
    }
}

fn send_and_exit(app: &Arc<App>, path: PathBuf) -> ! {
    let t0 = Instant::now();
    loop {
        let found = app.state().devices.first().map(|d| d.peer.clone());
        if let Some(peer) = found {
            eprintln!("found {} at {:?} after {:.1}s", peer.name, peer.addrs, t0.elapsed().as_secs_f64());
            app.state().selected = Some(peer.id);
            break;
        }
        if t0.elapsed() > FIND_DEVICE_WITHIN {
            eprintln!("no device found");
            std::process::exit(2);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    actions::add_items(app, vec![path]);
    actions::start_send(app);
    loop {
        std::thread::sleep(Duration::from_millis(200));
        let st = app.state();
        match st.transfers.first().and_then(|t| t.outcome.as_ref()) {
            Some(Ok(r)) => {
                eprintln!("sent {} files, {} bytes, wire {} in {:.2}s", r.files, r.bytes, r.wire, r.secs);
                std::process::exit(0);
            }
            Some(Err(e)) => {
                eprintln!("failed: {e}");
                std::process::exit(1);
            }
            None => {}
        }
    }
}
