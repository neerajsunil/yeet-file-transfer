//! Turns paths from the sender into safe local paths.

use crate::{Error, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

/// Maps sent paths into the destination folder. Top-level names that already exist are given
/// a free name ("Photos (2)") so nothing is overwritten; the choice sticks for the session.
#[derive(Debug)]
pub(super) struct Placer {
    dest: PathBuf,
    /// Top-level names as sent -> names used here.
    tops: HashMap<String, String>,
    saved: Vec<PathBuf>,
    made_dirs: HashSet<PathBuf>,
}

impl Placer {
    pub fn new(dest: PathBuf) -> Placer {
        Placer { dest, tops: HashMap::new(), saved: Vec::new(), made_dirs: HashSet::new() }
    }

    /// Top-level files and folders created so far, after renaming.
    pub fn saved(&self) -> &[PathBuf] {
        &self.saved
    }

    /// The local path for a sent `/`-separated relative path.
    pub fn place(&mut self, rel: &str) -> Result<PathBuf> {
        let (top, rest) = rel.split_once('/').unwrap_or((rel, ""));
        let local = match self.tops.get(top) {
            Some(t) => t.clone(),
            None => {
                safe_join(&self.dest, top)?; // validate before probing the filesystem
                let t = free_name(&self.dest, top);
                self.saved.push(self.dest.join(&t));
                self.tops.insert(top.to_string(), t.clone());
                t
            }
        };
        safe_join(&self.dest, &if rest.is_empty() { local } else { format!("{local}/{rest}") })
    }

    /// True the first time this session sees `dir`.
    pub fn mark_dir(&mut self, dir: &Path) -> bool {
        self.made_dirs.insert(dir.to_path_buf())
    }
}

/// `name`, or `name (2)`, `name (3)`... whichever doesn't exist in `dir` yet.
fn free_name(dir: &Path, name: &str) -> String {
    if !dir.join(name).exists() {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (2..)
        .map(|n| format!("{stem} ({n}){ext}"))
        .find(|candidate| !dir.join(candidate).exists())
        .expect("some numbered name is free")
}

/// Joins a sender-supplied relative path onto `dest`, refusing anything that could escape it:
/// `..`, absolute paths, drive letters, backslashes, empty parts.
fn safe_join(dest: &Path, rel: &str) -> Result<PathBuf> {
    let mut out = dest.to_path_buf();
    for part in rel.split('/') {
        let p = Path::new(part);
        let ok = !part.is_empty()
            && !part.contains(['\\', ':'])
            && matches!(p.components().next(), Some(Component::Normal(_)))
            && p.components().count() == 1;
        if !ok {
            return Err(Error::Unsafe(format!("path from sender would escape the destination: {rel}")));
        }
        out.push(part);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_accepts_normal_paths() {
        let d = Path::new("dest");
        assert_eq!(safe_join(d, "a/b c/d.txt").unwrap(), d.join("a").join("b c").join("d.txt"));
    }

    #[test]
    fn safe_join_refuses_escapes() {
        for bad in ["..", "a/../../b", "/etc/passwd", "C:/x", "c:", "a\\..\\b", "a//b", "", ".", "a/./b"] {
            assert!(safe_join(Path::new("dest"), bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn existing_top_level_names_get_numbered_once_per_session() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("Photos")).unwrap();
        std::fs::write(tmp.path().join("notes.txt"), "").unwrap();
        let mut p = Placer::new(tmp.path().to_path_buf());
        assert_eq!(p.place("Photos/a.jpg").unwrap(), tmp.path().join("Photos (2)").join("a.jpg"));
        assert_eq!(p.place("Photos/b.jpg").unwrap(), tmp.path().join("Photos (2)").join("b.jpg"));
        assert_eq!(p.place("notes.txt").unwrap(), tmp.path().join("notes (2).txt"));
        assert_eq!(p.place("new").unwrap(), tmp.path().join("new"));
        assert_eq!(p.saved().len(), 3);
    }
}
