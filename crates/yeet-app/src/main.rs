//! The yeet desktop app.
//!
//! - `state`: what the app knows, shared with background threads
//! - `actions` / `callbacks`: what the buttons do, and wiring them to the window
//! - `view`: mirrors the state into the window on a timer
//! - `network` / `receiver`: discovery and the receiving side
//! - `platform`: OS-specific bits (Defender, keep-awake, reveal in file manager)

// No console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod callbacks;
#[cfg(feature = "e2e")]
mod e2e;
mod format;
mod network;
mod platform;
mod receiver;
mod screenshots;
mod settings;
mod state;
mod text;
mod view;

slint::include_modules!();

use settings::Settings;
use slint::ComponentHandle;
use state::App;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// How often the window catches up with the app state.
const REFRESH_EVERY: Duration = Duration::from_millis(150);

fn main() {
    if let Some(dir) = std::env::var_os("YEET_SCREENSHOT") {
        screenshots::render_all(PathBuf::from(dir));
        return;
    }

    let settings = Settings::load();
    let bound = network::bind(settings.port);
    let app = Arc::new(App::new(settings, bound.as_ref().map_or(0, |(_, port)| *port)));
    match bound {
        Some((listener, _)) => {
            network::start_receiving(&app, listener);
            network::start_discovery(&app);
        }
        None => app.toast("Can't receive: no free network port. Is yeet already running?"),
    }
    let a = app.clone();
    std::thread::spawn(move || {
        let on = platform::defender_active();
        a.state().defender = on;
        let dir = a.state().settings.save_dir.clone();
        // Ready before anyone sends: what kind of drive received files land on.
        yeet_core::disk::Disk::probe(&dir);
        let short = platform::short_names_on(&dir);
        a.state().short_names = short;
    });

    let ui = AppWindow::new().expect("create window");
    let models = view::Models::new(&ui);
    callbacks::wire(&ui, &app);
    #[cfg(feature = "e2e")]
    e2e::start(&app);

    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    let mut awake = false;
    timer.start(slint::TimerMode::Repeated, REFRESH_EVERY, move || {
        let Some(ui) = weak.upgrade() else { return };
        let busy = view::refresh(&ui, &app, &models);
        if busy != awake {
            platform::keep_awake(busy);
            awake = busy;
        }
    });

    ui.run().expect("run window");
}
