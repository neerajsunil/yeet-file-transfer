use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

/// Counters for a running transfer, shared with whoever wants to show progress or cancel it.
#[derive(Debug, Default)]
pub struct Live {
    pub files_total: AtomicU64,
    pub bytes_total: AtomicU64,
    pub files_done: AtomicU64,
    /// Raw (uncompressed) bytes sent or written so far.
    pub bytes_done: AtomicU64,
    /// Bytes actually put on the network, after compression.
    pub wire_bytes: AtomicU64,
    /// Set once the receiver has accepted the offer.
    pub accepted: AtomicBool,
    /// Set by anyone to stop the transfer for good.
    pub cancel: AtomicBool,
    /// The connection dropped and the transfer is waiting to resume.
    pub reconnecting: AtomicBool,
}

impl Live {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Relaxed)
    }

    pub fn request_cancel(&self) {
        self.cancel.store(true, Relaxed);
    }
}

/// What a finished transfer did.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub files: u64,
    /// Raw bytes transferred.
    pub bytes: u64,
    /// Bytes on the wire (sender only; zero on the receiving side).
    pub wire: u64,
    pub secs: f64,
}
