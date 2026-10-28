use crate::args::{Args, Usage};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Compares every file under `a` with the same relative path under `b` by xxh3 hash.
pub fn run(args: &Args) -> Result<ExitCode, Usage> {
    let (a, b) = (Path::new(&args.positional()[0]), Path::new(&args.positional()[1]));
    let files: Vec<PathBuf> = walkdir::WalkDir::new(a)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect();
    let hash = |p: &Path| std::fs::read(p).ok().map(|d| xxhash_rust::xxh3::xxh3_128(&d));
    let bad: Vec<String> = files
        .par_iter()
        .filter_map(|f| {
            // `a` may itself be a file, in which case the relative path is empty.
            let rel = f.strip_prefix(a).expect("walked from a");
            let other = if rel.as_os_str().is_empty() { b.to_path_buf() } else { b.join(rel) };
            match (hash(f), hash(&other)) {
                (Some(x), Some(y)) if x == y => None,
                (_, None) => Some(format!("missing: {}", other.display())),
                _ => Some(format!("differs: {}", other.display())),
            }
        })
        .collect();
    for line in bad.iter().take(20) {
        println!("{line}");
    }
    println!("verify: {} files checked, {} bad", files.len(), bad.len());
    Ok(if bad.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}
