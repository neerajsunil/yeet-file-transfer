//! Wording shown to people: names, labels and error messages.

use std::path::Path;
use yeet_core::Error;

/// The last part of a path, for display.
pub fn file_name(p: &Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// "Photos" for one item, "3 items" for several.
pub fn what(names: &[String]) -> String {
    match names {
        [one] => one.clone(),
        _ => format!("{} items", names.len()),
    }
}

/// Turns an engine error into a sentence. `incoming` is whether this device was receiving.
pub fn describe(e: &Error, peer: &str, incoming: bool) -> String {
    match e {
        Error::Declined => format!("{peer} declined."),
        Error::Cancelled => "Cancelled.".into(),
        Error::CancelledByPeer => format!("{peer} cancelled."),
        Error::NoAnswer => format!("{peer} didn't respond."),
        Error::PeerDidNotReturn => format!("{peer} didn't reconnect in time."),
        Error::Unsafe(_) => format!("Stopped: {peer} sent something unsafe."),
        Error::Protocol(_) => format!("{peer} is running a different version of yeet."),
        e if e.is_disconnect() && !incoming => {
            format!("Couldn't reach {peer}. Check that yeet is open there and both are on the same network.")
        }
        e if e.is_disconnect() => format!("Lost the connection to {peer}."),
        Error::Io(io) if io.kind() == std::io::ErrorKind::StorageFull => "Not enough disk space.".into(),
        other => sentence(&other.to_string()),
    }
}

/// Capitalizes the first letter.
fn sentence(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or(String::new(), |f| f.to_uppercase().collect::<String>() + c.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(what(&["Photos".into()]), "Photos");
        assert_eq!(what(&["a".into(), "b".into()]), "2 items");
        assert_eq!(file_name(Path::new("/x/y/Game")), "Game");
    }

    #[test]
    fn errors_read_as_sentences() {
        assert_eq!(describe(&Error::Declined, "Laptop", false), "Laptop declined.");
        assert_eq!(describe(&Error::CancelledByPeer, "Laptop", true), "Laptop cancelled.");
        assert!(describe(&Error::Unreachable, "Laptop", false).starts_with("Couldn't reach Laptop"));
        assert_eq!(describe(&Error::LostContact, "Laptop", true), "Lost the connection to Laptop.");
        let other = Error::Io(std::io::Error::other("disk on fire"));
        assert_eq!(describe(&other, "Laptop", true), "Disk on fire");
    }
}
