//! Operating-system integrations. Each function works (or does nothing) on every platform,
//! so callers never need `cfg`.

use std::net::{IpAddr, UdpSocket};
use std::path::Path;
use std::process::Command;

/// Keeps the computer from sleeping while `on`. Must be called from the same (long-lived)
/// thread each time, since Windows tracks this per thread.
#[cfg(windows)]
pub fn keep_awake(on: bool) {
    #[link(name = "kernel32")]
    #[allow(unsafe_code)]
    extern "system" {
        fn SetThreadExecutionState(flags: u32) -> u32;
    }
    const ES_CONTINUOUS: u32 = 0x8000_0000;
    const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
    let flags = if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS };
    // SAFETY: takes a flags value and has no other preconditions.
    #[allow(unsafe_code)]
    unsafe {
        SetThreadExecutionState(flags);
    }
}

#[cfg(not(windows))]
pub fn keep_awake(_on: bool) {}

/// Opens the file manager with `path` selected.
pub fn reveal(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg("-R").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
    }
}

/// The address other machines on the LAN most likely reach this one at. Connecting a UDP
/// socket sends nothing; it just asks the OS which interface it would route through.
pub fn local_address() -> Option<IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    Some(s.local_addr().ok()?.ip())
}

/// Whether Microsoft Defender's real-time protection is on (it scans every file we write).
/// Slow (starts PowerShell), so call it off the UI thread.
#[cfg(windows)]
pub fn defender_active() -> bool {
    powershell("(Get-MpComputerStatus).RealTimeProtectionEnabled", false).is_some_and(|out| out.trim() == "True")
}

#[cfg(not(windows))]
pub fn defender_active() -> bool {
    false
}

/// Whether the volume holding `dir` still makes 8.3 short names ("PROGRA~1"), which makes
/// creating many files in one folder much slower. Slow (starts PowerShell).
#[cfg(windows)]
pub fn short_names_on(dir: &Path) -> bool {
    const NAME: &str = "yeet-short-name-probe.tmp";
    let _ = std::fs::create_dir_all(dir);
    let probe = dir.join(NAME);
    if std::fs::write(&probe, b"").is_err() {
        return false;
    }
    let cmd = format!(
        "(New-Object -ComObject Scripting.FileSystemObject).GetFile('{}').ShortName",
        probe.display().to_string().replace('\'', "''")
    );
    let out = powershell(&cmd, false);
    let _ = std::fs::remove_file(&probe);
    out.is_some_and(|s| !s.trim().is_empty() && !s.trim().eq_ignore_ascii_case(NAME))
}

#[cfg(not(windows))]
pub fn short_names_on(_dir: &Path) -> bool {
    false
}

/// Speeds up receiving into `dir`: asks Defender to skip it, and/or stops 8.3 short names on its
/// volume. One UAC prompt; returns false if it was declined or failed.
#[cfg(windows)]
pub fn defender_exclude(dir: &Path, defender: bool, short_names: bool) -> bool {
    let _ = std::fs::create_dir_all(dir);
    let path = dir.display().to_string();
    let mut steps = Vec::new();
    if defender {
        steps.push(format!("Add-MpPreference -ExclusionPath '{}'", path.replace('\'', "''")));
    }
    if short_names && path.as_bytes().get(1) == Some(&b':') {
        steps.push(format!("fsutil 8dot3name set {} 1 | Out-Null; if ($LASTEXITCODE) {{ exit 1 }}", &path[..2]));
    }
    let inner = steps.join("; ");
    let outer = format!(
        "$p = Start-Process powershell -Verb RunAs -Wait -PassThru -WindowStyle Hidden \
         -ArgumentList '-NoProfile','-EncodedCommand','{}'; exit $p.ExitCode",
        encode_command(&inner)
    );
    powershell(&outer, true).is_some()
}

#[cfg(not(windows))]
pub fn defender_exclude(_dir: &Path, _defender: bool, _short_names: bool) -> bool {
    false
}

/// Runs a PowerShell command without a console window. Returns stdout on success.
#[cfg(windows)]
fn powershell(command: &str, encoded: bool) -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut c = Command::new("powershell");
    c.args(["-NoProfile", "-NonInteractive"]);
    if encoded {
        c.args(["-EncodedCommand", &encode_command(command)]);
    } else {
        c.args(["-Command", command]);
    }
    let out = c.creation_flags(CREATE_NO_WINDOW).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// PowerShell's -EncodedCommand format (base64 of UTF-16LE), which sidesteps quoting.
#[cfg(windows)]
fn encode_command(s: &str) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ABC[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}
