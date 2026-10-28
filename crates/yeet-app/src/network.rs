//! Listening for transfers and finding devices.

use crate::receiver::Receiver;
use crate::state::{App, Device};
use yeet_core::discovery::{Discovery, PeerEvent};
use yeet_core::{recv, RECEIVE_WRITERS};
use std::net::TcpListener;
use std::sync::Arc;

/// How many ports after the configured one to try, so a second copy (or another app on the
/// port) doesn't stop receiving.
const PORT_TRIES: u16 = 20;

/// Binds the receive port, or the next free one.
pub fn bind(port: u16) -> Option<(TcpListener, u16)> {
    (port..port.saturating_add(PORT_TRIES)).find_map(|p| TcpListener::bind(("0.0.0.0", p)).ok().map(|l| (l, p)))
}

/// Accepts transfers in the background for the life of the app.
pub fn start_receiving(app: &Arc<App>, listener: TcpListener) {
    let handler = Receiver(app.clone());
    std::thread::spawn(move || {
        let _ = recv::serve(listener, RECEIVE_WRITERS, &handler);
    });
}

/// Announces this device and keeps `State::devices` up to date.
pub fn start_discovery(app: &Arc<App>) {
    let (id, name) = {
        let st = app.state();
        (st.settings.device_id.clone(), st.settings.device_name.clone())
    };
    let (discovery, events) = match Discovery::start(&id, &name, app.port) {
        Ok(d) => d,
        Err(e) => return app.toast(format!("Device discovery is unavailable: {e}")),
    };
    *app.discovery.lock().expect("poisoned") = Some(discovery);
    let app = app.clone();
    std::thread::spawn(move || {
        for event in events {
            let mut st = app.state();
            match event {
                PeerEvent::Found(peer) => match st.devices.iter_mut().find(|d| d.peer.id == peer.id) {
                    // Same device with a new address or name: update it in place.
                    Some(d) => d.peer = peer,
                    None => st.devices.push(Device { peer, manual: false }),
                },
                PeerEvent::Lost(id) => st.devices.retain(|d| d.manual || d.peer.id != id),
            }
            st.devices.sort_by_key(|d| (d.manual, d.peer.name.to_lowercase()));
        }
    });
}
