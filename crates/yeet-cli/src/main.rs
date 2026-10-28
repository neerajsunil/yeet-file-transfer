//! `yeet-cli`: send and receive from a terminal, plus tools for testing the engine.

mod args;
mod cmd;

use args::Args;
use std::process::ExitCode;

const USAGE: &str = "\
usage:
  yeet-cli receive <dest> [--port 7878] [--threads N] [--once]
  yeet-cli send <src>... --to <host:port> [--limit 100m|500m|1g] [--compress auto|none|zstdN] [--threads N] [--no-local]

developer tools:
  yeet-cli verify <a> <b>
      compare two trees by size and hash
  yeet-cli proxy <listen-port> <target host:port> --drop-after-mb N [--stall]
      forward connections, breaking the first one after N MB (reset, or a silent stall)";

fn main() -> ExitCode {
    let args = Args::parse(std::env::args().skip(1), &["--once", "--stall", "--no-local"]);
    let result = match (args.command(), args.positional().len()) {
        (Some("receive"), 1) => cmd::receive::run(&args),
        (Some("send"), n) if n > 0 => cmd::send::run(&args),
        (Some("verify"), 2) => cmd::verify::run(&args),
        (Some("proxy"), 2) => cmd::proxy::run(&args),
        _ => return usage(),
    };
    match result {
        Ok(code) => code,
        Err(args::Usage) => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}
