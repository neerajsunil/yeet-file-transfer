//! yeet's transfer engine: sends files and folders between computers on a LAN as fast as
//! the link allows, and survives the network dropping out.
//!
//! - [`send::send`] scans sources, offers them to a receiver, and streams them, compressing
//!   only what gets smaller. It reconnects and resumes if the connection drops.
//! - [`recv::serve`] accepts offers through a [`recv::Handler`] (where a UI asks the person)
//!   and writes what arrives, resuming interrupted transfers.
//! - [`discovery`] finds other yeet devices with mDNS.
//!
//! Progress is shared through [`Live`], which any thread can read or cancel.
//! See `docs/ARCHITECTURE.md` for how the pieces fit together.

pub mod discovery;
pub mod disk;
mod error;
mod live;
mod net;
pub mod protocol;
pub mod recv;
pub mod send;

pub use error::{Error, Result};
pub use live::{Live, Report};

/// A sensible worker count for this machine's CPU-bound work (reading and compressing).
pub fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get())
}

/// Threads writing received files. Creating a file waits on the file system rather than the
/// CPU, so more threads than cores keep the disk busy: on a laptop NVMe, 66k small files arrived
/// in 24 s with 6 writers, 15 s with 12 and 11 s with 32.
pub const RECEIVE_WRITERS: usize = 32;
