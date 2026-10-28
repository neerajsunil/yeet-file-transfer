//! Copying a file on this computer, for senders on the same machine: no socket, no chunks, just
//! the OS's own copy (the same one Explorer and robocopy use).

use crate::{Error, Result};
use std::path::Path;

/// Copies `src` to `dest`, overwriting it. `progress` gets the bytes copied so far and returns
/// false to cancel.
#[cfg(windows)]
#[allow(unsafe_code)] // CopyFileExW with a progress callback.
pub(super) fn copy_file(src: &Path, dest: &Path, progress: &mut dyn FnMut(u64) -> bool) -> Result<()> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::CopyFileExW;

    const PROGRESS_CONTINUE: u32 = 0;
    const PROGRESS_CANCEL: u32 = 1;

    unsafe extern "system" fn on_progress(
        _total: i64,
        done: i64,
        _stream_size: i64,
        _stream_done: i64,
        _stream: u32,
        _reason: u32,
        _src: HANDLE,
        _dest: HANDLE,
        data: *const c_void,
    ) -> u32 {
        // SAFETY: `data` is the `&mut &mut dyn FnMut` passed below, alive for the whole call.
        let f = unsafe { &mut *(data as *mut &mut dyn FnMut(u64) -> bool) };
        if f(done as u64) { PROGRESS_CONTINUE } else { PROGRESS_CANCEL }
    }

    let wide = |p: &Path| p.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let (s, d) = (wide(src), wide(dest));
    let mut f: &mut dyn FnMut(u64) -> bool = progress;
    // SAFETY: both paths are NUL-terminated; the callback only uses `f`, which outlives the call.
    let ok = unsafe {
        CopyFileExW(
            s.as_ptr(),
            d.as_ptr(),
            Some(on_progress),
            &mut f as *mut _ as *const c_void,
            std::ptr::null_mut(),
            0,
        )
    };
    if ok == 0 {
        let e = std::io::Error::last_os_error();
        // ERROR_REQUEST_ABORTED: the progress callback said stop.
        return Err(if e.raw_os_error() == Some(1235) { Error::Cancelled } else { Error::Io(e) });
    }
    Ok(())
}

#[cfg(not(windows))]
pub(super) fn copy_file(src: &Path, dest: &Path, progress: &mut dyn FnMut(u64) -> bool) -> Result<()> {
    let n = std::fs::copy(src, dest)?;
    if !progress(n) {
        return Err(Error::Cancelled);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_and_reports_progress() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dest) = (tmp.path().join("a"), tmp.path().join("b"));
        std::fs::write(&src, vec![7u8; 3 << 20]).unwrap();
        let mut last = 0;
        copy_file(&src, &dest, &mut |n| {
            last = n;
            true
        })
        .unwrap();
        assert_eq!(last, 3 << 20);
        assert_eq!(std::fs::read(&dest).unwrap(), vec![7u8; 3 << 20]);
    }

    #[test]
    fn can_be_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dest) = (tmp.path().join("a"), tmp.path().join("b"));
        std::fs::write(&src, vec![7u8; 3 << 20]).unwrap();
        assert!(matches!(copy_file(&src, &dest, &mut |_| false), Err(Error::Cancelled)));
    }
}
