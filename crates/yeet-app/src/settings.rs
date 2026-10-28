//! Persistent settings in the OS config folder (e.g. %APPDATA%\yeet\settings.json).
//! YEET_PROFILE=<name> keeps a separate copy, so two instances can run on one machine for testing.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The port yeet listens on unless it's taken. Other devices assume it when given a bare address.
pub const DEFAULT_PORT: u16 = 7878;

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    /// Stable identity for discovery; never shown.
    pub device_id: String,
    pub device_name: String,
    pub save_dir: PathBuf,
    pub compression: bool,
    pub port: u16,
    /// Folders yeet asked Windows Security to skip. Kept here because reading
    /// Defender's exclusion list needs admin rights.
    pub defender_excluded: Vec<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        let profile = profile();
        let base = dirs::download_dir().or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from("."));
        let folder = if profile.is_empty() { "yeet".to_string() } else { format!("yeet-{profile}") };
        let host = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "My Computer".into());
        Settings {
            device_id: format!("{:016x}", fastrand::u64(..)),
            device_name: if profile.is_empty() {
                pretty_host(&host)
            } else {
                format!("{} ({profile})", pretty_host(&host))
            },
            save_dir: base.join(folder),
            compression: true,
            port: if profile.is_empty() { DEFAULT_PORT } else { DEFAULT_PORT + 12 },
            defender_excluded: Vec::new(),
        }
    }
}

fn profile() -> String {
    std::env::var("YEET_PROFILE").unwrap_or_default()
}

/// "DESKTOP-JA8FPBF" -> "Desktop-JA8FPBF"; names that are already mixed case are kept.
fn pretty_host(h: &str) -> String {
    if h.chars().any(|c| c.is_lowercase()) {
        return h.to_string();
    }
    let mut out = String::new();
    for (i, part) in h.split('-').enumerate() {
        if i > 0 {
            out.push('-');
        }
        if i == 0 {
            let mut c = part.chars();
            if let Some(f) = c.next() {
                out.push(f);
                out.push_str(&c.as_str().to_lowercase());
            }
        } else {
            out.push_str(part);
        }
    }
    out
}

fn path() -> Option<PathBuf> {
    let p = profile();
    let dir = if p.is_empty() { "yeet".to_string() } else { format!("yeet-{p}") };
    dirs::config_dir().map(|d| d.join(dir).join("settings.json"))
}

impl Settings {
    /// Loads saved settings, creating and saving defaults on first run.
    pub fn load() -> Settings {
        let loaded =
            path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice::<Settings>(&b).ok());
        match loaded {
            Some(s) => s,
            None => {
                let s = Settings::default();
                s.save();
                s
            }
        }
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(p, json);
        }
    }
}
