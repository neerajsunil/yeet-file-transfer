//! A TCP forwarder that breaks the first connection, for testing reconnect and resume by hand
//! (the automated version lives in yeet-core's integration tests).

use crate::args::{Args, Usage};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;

pub fn run(args: &Args) -> Result<ExitCode, Usage> {
    let port: u16 = args.positional()[0].parse().map_err(|_| Usage)?;
    let target = args.positional()[1].clone();
    let drop_mb: u64 = args.parsed("--drop-after-mb", u64::MAX >> 20)?;
    let result = forward(port, &target, drop_mb << 20, args.has("--stall"));
    Ok(result.map_or_else(super::fail, |_| ExitCode::SUCCESS))
}

/// Forwards connections to `target`. The first one is broken after `limit` bytes from the
/// client: reset outright, or with `stall`, silently stops forwarding like a pulled cable.
fn forward(port: u16, target: &str, limit: u64, stall: bool) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    for (n, conn) in listener.incoming().enumerate() {
        let client = conn?;
        let server = TcpStream::connect(target)?;
        let limit = if n == 0 { limit } else { u64::MAX };
        let broken = Arc::new(AtomicBool::new(false));
        let (c2, s2, broken2) = (client.try_clone()?, server.try_clone()?, broken.clone());
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 << 10];
            let mut total = 0u64;
            while let Ok(k) = (&client).read(&mut buf) {
                if k == 0 || (&server).write_all(&buf[..k]).is_err() {
                    break;
                }
                total += k as u64;
                if total >= limit {
                    let how = if stall { "stall" } else { "reset" };
                    eprintln!("proxy: breaking connection {n} after {} MB ({how})", total >> 20);
                    broken.store(true, Relaxed);
                    if stall {
                        std::thread::sleep(Duration::from_secs(120));
                    }
                    break;
                }
            }
            let _ = client.shutdown(Shutdown::Both);
            let _ = server.shutdown(Shutdown::Both);
        });
        std::thread::spawn(move || {
            // Server -> client; once broken, swallow everything like a dead link would.
            let mut buf = vec![0u8; 64 << 10];
            while let Ok(k) = (&s2).read(&mut buf) {
                if k == 0 || (!broken2.load(Relaxed) && (&c2).write_all(&buf[..k]).is_err()) {
                    break;
                }
            }
            if !stall {
                let _ = c2.shutdown(Shutdown::Both);
            }
        });
    }
    Ok(())
}
