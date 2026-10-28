use crate::args::{Args, Usage};
use yeet_core::send::{self, Compress, SendOpts};
use yeet_core::{default_threads, Error, Live, Report};
use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

/// A receiver started at the same moment may not be listening yet; retry this long.
const STARTUP_RETRIES: u32 = 50;

pub fn run(args: &Args) -> Result<ExitCode, Usage> {
    let to = args.get("--to").ok_or(Usage)?.to_string();
    let compress = match args.get("--compress") {
        None | Some("auto") => Compress::Auto(3),
        Some("none") => Compress::None,
        Some(z) => Compress::Zstd(z.strip_prefix("zstd").and_then(|l| l.parse().ok()).ok_or(Usage)?),
    };
    let limit = args.get("--limit").map(|l| parse_rate(l).ok_or(Usage)).transpose()?;
    let o = SendOpts {
        sources: args.positional().iter().map(PathBuf::from).collect(),
        resolve: Arc::new(move || to.to_socket_addrs().map(|a| a.collect()).unwrap_or_default()),
        name: hostname(),
        limit,
        compress,
        threads: args.parsed("--threads", default_threads())?,
        prepared: None,
        local: !args.has("--no-local"),
    };

    let live = Live::default();
    let mut tries = 0;
    let report = loop {
        match send::send(&o, &live) {
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused && tries < STARTUP_RETRIES => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Ok(super::fail(e)),
            Ok(r) => break r,
        }
    };
    print_report(&report, limit);
    Ok(ExitCode::SUCCESS)
}

fn print_report(r: &Report, limit: Option<f64>) {
    let raw = r.bytes as f64;
    println!(
        "sent {} files, {:.2} GB raw -> {:.2} GB on wire ({:.1}%)",
        r.files,
        raw / 1e9,
        r.wire as f64 / 1e9,
        r.wire as f64 / raw.max(1.0) * 100.0
    );
    print!("time {:.1}s, effective {:.0} MB/s", r.secs, raw / 1e6 / r.secs);
    if let Some(l) = limit {
        // How close we got to the link's limit, with and without the benefit of compression.
        let wire_floor = r.wire as f64 / l;
        print!(
            " | uncompressed floor {:.1}s, own-wire floor {wire_floor:.1}s, link efficiency {:.0}%",
            raw / l,
            wire_floor / r.secs * 100.0
        );
    }
    println!();
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "yeet-cli".into())
}

/// Line rate like "1g" or "500m" (bits/s) to payload bytes/s, after ~6% TCP/IP overhead.
fn parse_rate(s: &str) -> Option<f64> {
    let s = s.to_lowercase();
    let (num, mult) = match s.chars().last()? {
        'g' => (&s[..s.len() - 1], 1e9),
        'm' => (&s[..s.len() - 1], 1e6),
        _ => (&s[..], 1.0),
    };
    Some(num.parse::<f64>().ok()? * mult / 8.0 * 0.94)
}

#[cfg(test)]
mod tests {
    use super::parse_rate;

    #[test]
    fn rates() {
        assert_eq!(parse_rate("1g"), Some(1e9 / 8.0 * 0.94));
        assert_eq!(parse_rate("100M"), Some(1e8 / 8.0 * 0.94));
        assert_eq!(parse_rate("fast"), None);
    }
}
