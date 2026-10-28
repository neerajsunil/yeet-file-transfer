//! Finds other yeet devices on the LAN with mDNS / DNS-SD, and announces this one.
//! Each device registers `<id>._yeet._tcp.local.` with its display name in a TXT record.

use crate::{Error, Result};
use crossbeam_channel::{unbounded, Receiver};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::net::{IpAddr, SocketAddr};

const SERVICE: &str = "_yeet._tcp.local.";

#[derive(Clone, Debug, PartialEq)]
pub struct Peer {
    pub id: String,
    pub name: String,
    /// Best address first (IPv4 before IPv6).
    pub addrs: Vec<SocketAddr>,
}

#[derive(Debug)]
pub enum PeerEvent {
    Found(Peer),
    Lost(String),
}

/// Announces this device and reports others until dropped.
pub struct Discovery {
    daemon: ServiceDaemon,
    id: String,
    port: u16,
}

fn err(e: impl std::fmt::Display) -> Error {
    Error::Io(std::io::Error::other(format!("discovery: {e}")))
}

impl Discovery {
    /// Announces this device and starts browsing. Events for this device itself are filtered out.
    pub fn start(id: &str, name: &str, port: u16) -> Result<(Discovery, Receiver<PeerEvent>)> {
        let daemon = ServiceDaemon::new().map_err(err)?;
        let d = Discovery { daemon, id: id.to_string(), port };
        d.register(name)?;

        let browse = d.daemon.browse(SERVICE).map_err(err)?;
        let (tx, rx) = unbounded();
        let me = id.to_string();
        std::thread::spawn(move || {
            while let Ok(event) = browse.recv() {
                let out = match event {
                    ServiceEvent::ServiceResolved(info) => {
                        let id = instance_of(info.get_fullname());
                        if id == me {
                            continue;
                        }
                        let name = info.get_property_val_str("name").unwrap_or(&id).to_string();
                        let mut ips: Vec<IpAddr> = info.get_addresses().iter().map(|a| a.to_ip_addr()).collect();
                        ips.sort_by_key(|ip| (ip.is_ipv6(), *ip));
                        let addrs = ips.into_iter().map(|ip| SocketAddr::new(ip, info.get_port())).collect();
                        PeerEvent::Found(Peer { id, name, addrs })
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => {
                        let id = instance_of(&fullname);
                        if id == me {
                            continue;
                        }
                        PeerEvent::Lost(id)
                    }
                    _ => continue,
                };
                if tx.send(out).is_err() {
                    break;
                }
            }
        });
        Ok((d, rx))
    }

    /// Re-announces under a new display name.
    pub fn set_name(&self, name: &str) -> Result<()> {
        self.register(name)
    }

    fn register(&self, name: &str) -> Result<()> {
        let host = format!("yeet-{}.local.", self.id);
        let props = [("name", name), ("v", "1")];
        let info =
            ServiceInfo::new(SERVICE, &self.id, &host, "", self.port, &props[..]).map_err(err)?.enable_addr_auto();
        self.daemon.register(info).map_err(err)
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

fn instance_of(fullname: &str) -> String {
    fullname.strip_suffix(SERVICE).unwrap_or(fullname).trim_end_matches('.').to_string()
}
