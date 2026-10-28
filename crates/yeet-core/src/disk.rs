//! What kind of drive a path is on, so both ends can pace their disk work to it.
//!
//! A spinning disk wants few, long, in-order reads and writes; an SSD wants many at once. FAT
//! can't hold files of 4 GiB or more and zero-fills space reserved ahead of writing, so nothing
//! is reserved there. Probing takes a few milliseconds and is cached per drive, and [`prewarm`]
//! does it in the background while the person is still choosing what to send.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Media {
    Unknown = 0,
    /// Spinning disk: seeks are expensive.
    Hdd = 1,
    Ssd = 2,
    /// A network share: latency per request, so keep many in flight.
    Network = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fs {
    Other = 0,
    Ntfs = 1,
    ReFs = 2,
    ExFat = 3,
    /// FAT12/16/32.
    Fat = 4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Disk {
    pub media: Media,
    pub fs: Fs,
}

impl Disk {
    pub const UNKNOWN: Disk = Disk { media: Media::Unknown, fs: Fs::Other };

    /// The drive `path` is on. `path` need not exist yet; its nearest existing parent is used.
    pub fn probe(path: &Path) -> Disk {
        let Some(volume) = volume_of(path) else { return Disk::UNKNOWN };
        static CACHE: OnceLock<Mutex<HashMap<PathBuf, Disk>>> = OnceLock::new();
        let cache = CACHE.get_or_init(Mutex::default);
        if let Some(d) = cache.lock().expect("disk cache poisoned").get(&volume) {
            return *d;
        }
        let d = sys::probe(&volume);
        cache.lock().expect("disk cache poisoned").insert(volume, d);
        d
    }

    /// The slowest kind of drive among `paths`, which is the one to pace for.
    pub fn probe_all<'a>(paths: impl IntoIterator<Item = &'a PathBuf>) -> Disk {
        paths.into_iter().map(|p| Disk::probe(p)).max_by_key(|d| d.media == Media::Hdd).unwrap_or(Disk::UNKNOWN)
    }

    pub fn spinning(&self) -> bool {
        self.media == Media::Hdd
    }

    /// Whether to reserve a file's full size before writing it.
    pub fn preallocate(&self) -> bool {
        self.fs != Fs::Fat
    }

    /// Largest file this drive's format can hold.
    pub fn max_file(&self) -> u64 {
        if self.fs == Fs::Fat {
            u32::MAX as u64
        } else {
            u64::MAX
        }
    }

    pub fn encode(&self) -> [u8; 2] {
        [self.media as u8, self.fs as u8]
    }

    /// Unknown values (from a newer peer) read as "unknown", never as an error.
    pub fn decode(b: &[u8]) -> Disk {
        let media = match b.first() {
            Some(1) => Media::Hdd,
            Some(2) => Media::Ssd,
            Some(3) => Media::Network,
            _ => Media::Unknown,
        };
        let fs = match b.get(1) {
            Some(1) => Fs::Ntfs,
            Some(2) => Fs::ReFs,
            Some(3) => Fs::ExFat,
            Some(4) => Fs::Fat,
            _ => Fs::Other,
        };
        Disk { media, fs }
    }
}

/// Probes the drives of `paths` on a background thread, so the answer is cached by the time a
/// transfer needs it.
pub fn prewarm(paths: Vec<PathBuf>) {
    std::thread::spawn(move || {
        for p in &paths {
            Disk::probe(p);
        }
    });
}

/// Disk work in flight at once.
pub(crate) fn readers(src: &Disk, cpu_threads: usize) -> usize {
    match src.media {
        // Enough to keep compression busy while the head moves mostly forward.
        Media::Hdd => 4,
        _ => cpu_threads.max(32),
    }
}

pub(crate) fn writers(dest: &Disk, requested: usize) -> usize {
    match dest.media {
        Media::Hdd => requested.min(8),
        _ => requested,
    }
}

/// The root of the drive holding `path` (or its nearest existing parent).
fn volume_of(path: &Path) -> Option<PathBuf> {
    let existing = path.ancestors().find(|p| p.exists())?;
    let full = std::fs::canonicalize(existing).ok()?;
    sys::volume_root(&full)
}

#[cfg(windows)]
#[allow(unsafe_code)] // Win32 calls; each is checked for failure and falls back to "unknown".
mod sys {
    use super::{Disk, Fs, Media};
    use std::ffi::c_void;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetDriveTypeW, GetVolumeInformationW, GetVolumePathNameW, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const DRIVE_REMOTE: u32 = 4;
    const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
    const STORAGE_DEVICE_SEEK_PENALTY_PROPERTY: i32 = 7;
    const PROPERTY_STANDARD_QUERY: i32 = 0;

    #[repr(C)]
    struct PropertyQuery {
        property_id: i32,
        query_type: i32,
        additional: [u8; 4],
    }

    #[repr(C)]
    #[derive(Default)]
    struct SeekPenalty {
        version: u32,
        size: u32,
        incurs_seek_penalty: u8,
    }

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(Some(0)).collect()
    }

    fn from_wide(buf: &[u16]) -> std::ffi::OsString {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        std::ffi::OsString::from_wide(&buf[..end])
    }

    pub fn volume_root(path: &Path) -> Option<PathBuf> {
        let p = wide(path.as_os_str());
        let mut out = [0u16; 1024];
        // SAFETY: `p` is NUL-terminated and `out`'s length is passed.
        let ok = unsafe { GetVolumePathNameW(p.as_ptr(), out.as_mut_ptr(), out.len() as u32) };
        (ok != 0).then(|| PathBuf::from(from_wide(&out)))
    }

    pub fn probe(root: &Path) -> Disk {
        let r = wide(root.as_os_str());
        // SAFETY: `r` is NUL-terminated.
        if unsafe { GetDriveTypeW(r.as_ptr()) } == DRIVE_REMOTE {
            return Disk { media: Media::Network, fs: fs(&r) };
        }
        Disk { media: media(root).unwrap_or(Media::Unknown), fs: fs(&r) }
    }

    fn fs(root: &[u16]) -> Fs {
        let mut name = [0u16; 64];
        // SAFETY: `root` is NUL-terminated; unused outputs are null, and `name`'s length is passed.
        let ok = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                name.as_mut_ptr(),
                name.len() as u32,
            )
        };
        if ok == 0 {
            return Fs::Other;
        }
        match from_wide(&name).to_string_lossy().to_ascii_uppercase().as_str() {
            "NTFS" => Fs::Ntfs,
            "REFS" => Fs::ReFs,
            "EXFAT" => Fs::ExFat,
            "FAT" | "FAT32" | "FAT16" | "FAT12" => Fs::Fat,
            _ => Fs::Other,
        }
    }

    /// Asks the volume's disk whether it incurs a seek penalty. Needs no admin rights: the
    /// volume is opened without read or write access.
    fn media(root: &Path) -> Option<Media> {
        // Canonical paths are verbatim (`\\?\C:\`); only the letter matters here.
        let letter = root.to_str()?.trim_start_matches(r"\\?\").trim_end_matches('\\');
        if letter.len() != 2 || !letter.ends_with(':') {
            return None; // mounted folders and the like: not worth guessing
        }
        let device = wide(std::ffi::OsStr::new(&format!(r"\\.\{letter}")));
        // SAFETY: `device` is NUL-terminated; no security attributes or template.
        let h = unsafe {
            CreateFileW(
                device.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return None;
        }
        let query = PropertyQuery {
            property_id: STORAGE_DEVICE_SEEK_PENALTY_PROPERTY,
            query_type: PROPERTY_STANDARD_QUERY,
            additional: [0; 4],
        };
        let mut out = SeekPenalty::default();
        let mut got = 0u32;
        // SAFETY: buffers are live locals with their sizes passed; `h` is a valid handle.
        let ok = unsafe {
            DeviceIoControl(
                h,
                IOCTL_STORAGE_QUERY_PROPERTY,
                &query as *const _ as *const c_void,
                std::mem::size_of::<PropertyQuery>() as u32,
                &mut out as *mut _ as *mut c_void,
                std::mem::size_of::<SeekPenalty>() as u32,
                &mut got,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: `h` came from CreateFileW and is closed once.
        unsafe { CloseHandle(h) };
        (ok != 0 && got as usize >= std::mem::size_of::<SeekPenalty>() - 3).then_some(if out.incurs_seek_penalty != 0 {
            Media::Hdd
        } else {
            Media::Ssd
        })
    }
}

#[cfg(not(windows))]
mod sys {
    use super::{Disk, Fs, Media};
    use std::path::{Path, PathBuf};

    /// Without a portable volume API, every path counts as its own drive.
    pub fn volume_root(path: &Path) -> Option<PathBuf> {
        Some(path.to_path_buf())
    }

    pub fn probe(_: &Path) -> Disk {
        Disk { media: Media::Unknown, fs: Fs::Other }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_tolerates_unknown_values() {
        let d = Disk { media: Media::Hdd, fs: Fs::Fat };
        assert_eq!(Disk::decode(&d.encode()), d);
        assert_eq!(Disk::decode(&[9, 9]), Disk::UNKNOWN);
        assert_eq!(Disk::decode(&[]), Disk::UNKNOWN);
    }

    #[test]
    fn fat_limits() {
        let fat = Disk { media: Media::Ssd, fs: Fs::Fat };
        assert!(!fat.preallocate());
        assert_eq!(fat.max_file(), 4 * 1024 * 1024 * 1024 - 1);
        assert!(Disk { media: Media::Ssd, fs: Fs::ExFat }.preallocate());
    }

    #[test]
    fn spinning_disks_get_less_parallel_io() {
        let hdd = Disk { media: Media::Hdd, fs: Fs::Ntfs };
        let ssd = Disk { media: Media::Ssd, fs: Fs::Ntfs };
        assert!(readers(&hdd, 16) < readers(&ssd, 16));
        assert!(writers(&hdd, 32) < writers(&ssd, 32));
    }

    #[test]
    fn probes_a_path_that_does_not_exist_yet() {
        let tmp = tempfile::tempdir().unwrap();
        // Same drive as its existing parent, whatever kind that is.
        assert_eq!(Disk::probe(&tmp.path().join("not/yet")), Disk::probe(tmp.path()));
    }
}
