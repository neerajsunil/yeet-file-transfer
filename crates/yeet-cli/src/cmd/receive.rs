use crate::args::{Args, Usage};
use yeet_core::recv::{self, Handler, Offer};
use yeet_core::{Live, Report, Result, RECEIVE_WRITERS};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub fn run(args: &Args) -> std::result::Result<ExitCode, Usage> {
    let handler = AutoAccept(PathBuf::from(&args.positional()[0]));
    // Learn what the destination drive is while waiting for a sender.
    yeet_core::disk::prewarm(vec![handler.0.clone()]);
    let port: u16 = args.parsed("--port", 7878)?;
    let threads = args.parsed("--threads", RECEIVE_WRITERS)?;
    let listener = match TcpListener::bind(("0.0.0.0", port)) {
        Ok(l) => l,
        Err(e) => return Ok(super::fail(e)),
    };
    let result = if args.has("--once") {
        listener.accept().map_err(Into::into).and_then(|(s, peer)| recv::handle(s, peer, threads, &handler))
    } else {
        recv::serve(listener, threads, &handler)
    };
    Ok(result.map_or_else(super::fail, |_| ExitCode::SUCCESS))
}

/// Accepts every offer into one folder: there's no one to ask.
struct AutoAccept(PathBuf);

impl Handler for AutoAccept {
    fn offer(&self, o: &Offer, _: &AtomicBool) -> Option<PathBuf> {
        println!("offer from {} ({}): {} files, {:.2} GB", o.name, o.peer, o.files, o.bytes as f64 / 1e9);
        Some(self.0.clone())
    }

    fn started(&self, _: &Offer, _: Arc<Live>) {}

    fn finished(&self, _: &Offer, r: &Result<Report>, _: &[PathBuf]) {
        match r {
            Ok(r) => println!(
                "received {} files, {:.2} GB in {:.1}s ({:.0} MB/s)",
                r.files,
                r.bytes as f64 / 1e9,
                r.secs,
                r.bytes as f64 / 1e6 / r.secs
            ),
            Err(e) => eprintln!("transfer failed: {e}"),
        }
    }
}
