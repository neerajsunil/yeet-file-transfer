//! One module per command. Each `run` returns the process exit code, or `Usage` for bad arguments.

pub mod proxy;
pub mod receive;
pub mod send;
pub mod verify;

use std::process::ExitCode;

/// Prints an engine or I/O error and turns it into a failing exit code.
fn fail(e: impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {e}");
    ExitCode::FAILURE
}
