//! Turns the chosen files and folders into a flat, ordered list of what to send.

use crate::{Error, Result};
use std::path::{Path, PathBuf};

/// One file or directory to send.
#[derive(Debug)]
pub(super) struct Entry {
    /// Path as the receiver should see it: `/`-separated, starting with the source's name.
    pub rel: String,
    pub path: PathBuf,
    pub size: u64,
    pub dir: bool,
}

/// Walks every source in a stable order (so file ids match across reconnects).
/// Symlinks and special files are skipped.
pub(super) fn scan(sources: &[PathBuf]) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    for src in sources {
        let name = utf8_name(src, src.file_name().and_then(|n| n.to_str()))?;
        for entry in walkdir::WalkDir::new(src).sort_by_file_name() {
            let e = entry?;
            let ft = e.file_type();
            if !ft.is_dir() && !ft.is_file() {
                continue;
            }
            let mut rel = name.to_string();
            let inner = e.path().strip_prefix(src).map_err(|_| Error::protocol("path outside source"))?;
            for c in inner.components() {
                rel.push('/');
                rel.push_str(utf8_name(e.path(), c.as_os_str().to_str())?);
            }
            let size = if ft.is_file() { e.metadata()?.len() } else { 0 };
            out.push(Entry { rel, path: e.into_path(), size, dir: ft.is_dir() });
        }
    }
    Ok(out)
}

fn utf8_name<'a>(path: &Path, name: Option<&'a str>) -> Result<&'a str> {
    name.ok_or_else(|| {
        Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("can't send {}: its name isn't valid Unicode", path.display()),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn lists_dirs_and_files_with_relative_paths_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("game");
        fs::create_dir_all(root.join("b/c")).unwrap();
        fs::write(root.join("b/c/x.bin"), [0u8; 10]).unwrap();
        fs::write(root.join("a.txt"), "hi").unwrap();
        let single = tmp.path().join("notes.md");
        fs::write(&single, "n").unwrap();

        let got: Vec<(String, u64, bool)> =
            scan(&[root, single]).unwrap().into_iter().map(|e| (e.rel, e.size, e.dir)).collect();
        assert_eq!(
            got,
            [
                ("game".into(), 0, true),
                ("game/a.txt".into(), 2, false),
                ("game/b".into(), 0, true),
                ("game/b/c".into(), 0, true),
                ("game/b/c/x.bin".into(), 10, false),
                ("notes.md".into(), 1, false),
            ]
        );
    }
}
