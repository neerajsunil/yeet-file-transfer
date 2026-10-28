//! yeet-bench: measures which compression strategy moves a folder fastest
//! over a given link speed, using real data sampled from the folder.
//!
//! Usage:
//!   yeet-bench scan <folder> [--sample-mb N] [--threads T]
//!   yeet-bench write-test <dir> [--files N] [--size-kb K]

use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Unit of transfer: large files are cut into blocks, small files are bundled into blocks.
const BLOCK: usize = 4 << 20;
/// Files below this size get bundled together (per extension) rather than sent alone.
const SMALL: u64 = 1 << 20;
/// Classes below this share of total bytes are folded into "other".
const MIN_CLASS_SHARE: f64 = 0.005;
/// A cheaper codec wins if it is within this fraction of the best time.
const TIE_MARGIN: f64 = 0.03;

const MB: f64 = 1_000_000.0;
const MIB: f64 = 1_048_576.0;

/// Payload throughput in bytes/s after TCP/IP overhead (~94% of line rate).
const LINKS: [(&str, f64); 4] =
    [("100M", 100e6 / 8.0 * 0.94), ("1G", 1e9 / 8.0 * 0.94), ("2.5G", 2.5e9 / 8.0 * 0.94), ("10G", 10e9 / 8.0 * 0.94)];

#[derive(Clone, Copy, PartialEq)]
enum Codec {
    None,
    Lz4,
    Zstd(i32),
}

/// Ordered from cheapest to most expensive; ties go to the earlier entry.
const CODECS: [Codec; 9] = [
    Codec::None,
    Codec::Lz4,
    Codec::Zstd(-5),
    Codec::Zstd(-1),
    Codec::Zstd(1),
    Codec::Zstd(3),
    Codec::Zstd(6),
    Codec::Zstd(9),
    Codec::Zstd(12),
];

impl Codec {
    fn name(self) -> String {
        match self {
            Codec::None => "none".into(),
            Codec::Lz4 => "lz4".into(),
            Codec::Zstd(l) => format!("zstd{l}"),
        }
    }

    fn compress(self, src: &[u8]) -> Vec<u8> {
        match self {
            Codec::None => src.to_vec(),
            Codec::Lz4 => lz4_flex::block::compress(src),
            Codec::Zstd(l) => zstd::bulk::compress(src, l).expect("zstd compress"),
        }
    }

    fn decompress(self, src: &[u8], len: usize) -> Vec<u8> {
        match self {
            Codec::None => src.to_vec(),
            Codec::Lz4 => lz4_flex::block::decompress(src, len).expect("lz4 decompress"),
            Codec::Zstd(_) => zstd::bulk::decompress(src, len).expect("zstd decompress"),
        }
    }
}

struct Entry {
    path: PathBuf,
    size: u64,
    class: String,
}

struct Block {
    class: String,
    data: Vec<u8>,
}

/// Where one sampled block comes from: one or more (file, offset, len) pieces.
struct ReadPlan {
    class: String,
    pieces: Vec<(PathBuf, u64, usize)>,
}

/// Per class, per codec: sample bytes in, bytes out, compress/decompress CPU seconds.
#[derive(Default, Clone, Copy)]
struct Cost {
    raw: f64,
    out: f64,
    cpu_c: f64,
    cpu_d: f64,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str, default: usize| -> usize {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    match (args.first().map(String::as_str), args.get(1)) {
        (Some("scan"), Some(dir)) => {
            let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
            let threads = flag("--threads", cores);
            scan_cmd(Path::new(dir), flag("--sample-mb", 1024), threads);
        }
        (Some("write-test"), Some(dir)) => write_test(Path::new(dir), flag("--files", 5000), flag("--size-kb", 16)),
        _ => {
            eprintln!("usage: yeet-bench scan <folder> [--sample-mb N] [--threads T]");
            eprintln!("       yeet-bench write-test <dir> [--files N] [--size-kb K]");
            std::process::exit(2);
        }
    }
}

fn scan_cmd(root: &Path, sample_mb: usize, threads: usize) {
    println!("== {} ==", root.display());

    // 1. Enumerate.
    let t = Instant::now();
    let mut entries: Vec<Entry> = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            let size = e.metadata().map_or(0, |m| m.len());
            let class = e.path().extension().map_or("(none)".into(), |x| x.to_string_lossy().to_lowercase());
            Entry { path: e.into_path(), size, class }
        })
        .collect();
    let scan_secs = t.elapsed().as_secs_f64();
    let total: u64 = entries.iter().map(|e| e.size).sum();
    println!(
        "scan: {} files, {:.2} GB in {:.2}s ({:.0} files/s)",
        entries.len(),
        total as f64 / 1e9,
        scan_secs,
        entries.len() as f64 / scan_secs
    );
    print_size_histogram(&entries, total);

    // Fold tiny classes into "other" so the report stays readable.
    let mut class_bytes: HashMap<String, u64> = HashMap::new();
    for e in &entries {
        *class_bytes.entry(e.class.clone()).or_default() += e.size;
    }
    for e in &mut entries {
        if (class_bytes[&e.class] as f64) < total as f64 * MIN_CLASS_SHARE {
            e.class = "other".into();
        }
    }
    let mut by_class: HashMap<String, Vec<&Entry>> = HashMap::new();
    for e in &entries {
        by_class.entry(e.class.clone()).or_default().push(e);
    }

    // 2. Sample and read.
    let plans = plan_samples(&by_class, total, sample_mb as u64 * (1 << 20));
    let t = Instant::now();
    let blocks: Vec<Block> = plans.par_iter().map(read_block).collect();
    let read_secs = t.elapsed().as_secs_f64();
    let sample_bytes: usize = blocks.iter().map(|b| b.data.len()).sum();
    let piece_count: usize = plans.iter().map(|p| p.pieces.len()).sum();
    println!(
        "sample: {:.0} MiB in {} blocks from {} file reads, read at {:.0} MB/s ({:.2}s, parallel random reads)",
        sample_bytes as f64 / MIB,
        blocks.len(),
        piece_count,
        sample_bytes as f64 / MB / read_secs,
        read_secs
    );

    // 3. Run every codec over every block.
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
    let mut costs: Vec<HashMap<String, Cost>> = Vec::new();
    println!("\ncodec      ratio   compress MB/s   decompress MB/s   ({threads} threads, measured)");
    for codec in CODECS {
        let (per_block, wall_c, wall_d) = pool.install(|| bench_codec(codec, &blocks));
        let mut by: HashMap<String, Cost> = HashMap::new();
        let (mut raw, mut out) = (0.0, 0.0);
        for (b, c) in blocks.iter().zip(&per_block) {
            let e = by.entry(b.class.clone()).or_default();
            e.raw += c.raw;
            e.out += c.out;
            e.cpu_c += c.cpu_c;
            e.cpu_d += c.cpu_d;
            raw += c.raw;
            out += c.out;
        }
        if codec != Codec::None {
            println!(
                "{:<9} {:>6.3}   {:>13.0}   {:>15.0}",
                codec.name(),
                out / raw,
                raw / MB / wall_c,
                raw / MB / wall_d
            );
        }
        costs.push(by);
    }

    // Blocks where even zstd1 can't save 3% should be sent raw without trying hard.
    let z1 = CODECS.iter().position(|c| *c == Codec::Zstd(1)).unwrap();
    let incompressible: f64 = pool.install(|| {
        blocks
            .par_iter()
            .filter(|b| (Codec::Zstd(1).compress(&b.data).len() as f64) > b.data.len() as f64 * 0.97)
            .map(|b| b.data.len() as f64)
            .sum()
    });
    println!(
        "\nincompressible blocks (zstd1 saves <3%): {:.0}% of sampled bytes",
        (incompressible / sample_bytes as f64 * 100.0).max(0.0)
    );

    // Scale each class's sample up to its real size in the folder.
    let mut classes: Vec<(String, f64)> = by_class
        .iter()
        .map(|(c, es)| (c.clone(), es.iter().map(|e| e.size).sum::<u64>() as f64))
        .filter(|(c, _)| costs[0].contains_key(c))
        .collect();
    classes.sort_by(|a, b| b.1.total_cmp(&a.1));

    print_class_table(&classes, &by_class, &costs, total, threads, z1);
    print_strategies(&classes, &costs, total, threads);
}

fn print_size_histogram(entries: &[Entry], total: u64) {
    let buckets: [(&str, u64); 6] = [
        ("<4K", 4 << 10),
        ("4K-64K", 64 << 10),
        ("64K-1M", 1 << 20),
        ("1M-16M", 16 << 20),
        ("16M-256M", 256 << 20),
        (">256M", u64::MAX),
    ];
    let mut counts = [(0usize, 0u64); 6];
    for e in entries {
        let i = buckets.iter().position(|(_, lim)| e.size < *lim).unwrap();
        counts[i].0 += 1;
        counts[i].1 += e.size;
    }
    print!("sizes:");
    for ((name, _), (n, b)) in buckets.iter().zip(counts) {
        print!("  {name}: {n} files/{:.1}%", b as f64 / total.max(1) as f64 * 100.0);
    }
    println!();
}

/// Each class gets a share of the sample budget proportional to its bytes (min 16 MiB).
/// Large files contribute random 4 MiB blocks; small files are bundled into 4 MiB blocks.
fn plan_samples(by_class: &HashMap<String, Vec<&Entry>>, total: u64, budget: u64) -> Vec<ReadPlan> {
    let mut rng = fastrand::Rng::with_seed(42);
    let mut plans = Vec::new();
    let mut names: Vec<&String> = by_class.keys().collect();
    names.sort();
    for class in names {
        let files = &by_class[class];
        let class_bytes: u64 = files.iter().map(|e| e.size).sum();
        if class_bytes == 0 {
            continue;
        }
        let target = (budget as f64 * class_bytes as f64 / total as f64) as u64;
        let target = target.max(16 << 20).min(class_bytes);
        let (small, large): (Vec<&&Entry>, Vec<&&Entry>) = files.iter().partition(|e| e.size < SMALL);
        let small_bytes: u64 = small.iter().map(|e| e.size).sum();
        let small_target = (target as f64 * small_bytes as f64 / class_bytes as f64) as u64;
        let large_target = target - small_target;

        // Large: pick random block-aligned offsets, weighted by file size.
        let mut prefix = Vec::with_capacity(large.len());
        let mut acc = 0u64;
        for e in &large {
            acc += e.size;
            prefix.push(acc);
        }
        let mut seen = HashSet::new();
        let (mut got, mut tries) = (0u64, 0);
        while got < large_target && acc > 0 && tries < 100_000 {
            tries += 1;
            let off = rng.u64(0..acc);
            let i = prefix.partition_point(|&p| p <= off);
            let start = if i == 0 { 0 } else { prefix[i - 1] };
            let blk = (off - start) / BLOCK as u64 * BLOCK as u64;
            if !seen.insert((i, blk)) {
                continue;
            }
            let len = (large[i].size - blk).min(BLOCK as u64) as usize;
            got += len as u64;
            plans.push(ReadPlan { class: class.clone(), pieces: vec![(large[i].path.clone(), blk, len)] });
        }

        // Small: shuffle and pack into bundles.
        let mut order: Vec<usize> = (0..small.len()).collect();
        rng.shuffle(&mut order);
        let (mut got, mut pieces, mut cur) = (0u64, Vec::new(), 0usize);
        for i in order {
            if got >= small_target {
                break;
            }
            let e = small[i];
            pieces.push((e.path.clone(), 0, e.size as usize));
            cur += e.size as usize;
            got += e.size;
            if cur >= BLOCK {
                plans.push(ReadPlan { class: class.clone(), pieces: std::mem::take(&mut pieces) });
                cur = 0;
            }
        }
        if !pieces.is_empty() {
            plans.push(ReadPlan { class: class.clone(), pieces });
        }
    }
    plans
}

fn read_block(plan: &ReadPlan) -> Block {
    let mut data = Vec::new();
    for (path, off, len) in &plan.pieces {
        let Ok(mut f) = File::open(path) else { continue };
        let start = data.len();
        data.resize(start + len, 0);
        let ok = f.seek(SeekFrom::Start(*off)).is_ok() && f.read_exact(&mut data[start..]).is_ok();
        if !ok {
            data.truncate(start);
        }
    }
    Block { class: plan.class.clone(), data }
}

/// Returns per-block costs plus wall-clock seconds for compress and decompress passes.
fn bench_codec(codec: Codec, blocks: &[Block]) -> (Vec<Cost>, f64, f64) {
    let t = Instant::now();
    let compressed: Vec<(Vec<u8>, f64)> = blocks
        .par_iter()
        .map(|b| {
            let t = Instant::now();
            let c = codec.compress(&b.data);
            (c, t.elapsed().as_secs_f64())
        })
        .collect();
    let wall_c = t.elapsed().as_secs_f64();

    let t = Instant::now();
    let cpu_d: Vec<f64> = blocks
        .par_iter()
        .zip(&compressed)
        .map(|(b, (c, _))| {
            let t = Instant::now();
            let d = codec.decompress(c, b.data.len());
            assert_eq!(d.len(), b.data.len());
            t.elapsed().as_secs_f64()
        })
        .collect();
    let wall_d = t.elapsed().as_secs_f64();

    let costs = blocks
        .iter()
        .zip(&compressed)
        .zip(&cpu_d)
        .map(|((b, (c, tc)), td)| {
            // "none" still costs a memcpy in this harness; a real sender pays nothing.
            let none = codec == Codec::None;
            Cost {
                raw: b.data.len() as f64,
                out: c.len() as f64,
                cpu_c: if none { 0.0 } else { *tc },
                cpu_d: if none { 0.0 } else { *td },
            }
        })
        .collect();
    (costs, wall_c, wall_d)
}

/// Seconds to move `class_total` bytes of a class with this codec, if it were alone in the pipe.
/// Sender compress, wire and receiver decompress all overlap, so the slowest stage wins.
fn stage_secs(c: &Cost, class_total: f64, threads: usize, link: f64) -> (f64, f64, f64) {
    let scale = class_total / c.raw;
    (c.cpu_c * scale / threads as f64, c.out * scale / link, c.cpu_d * scale / threads as f64)
}

/// Picks the cheapest codec within TIE_MARGIN of the fastest for one class.
fn pick(class: &str, class_total: f64, costs: &[HashMap<String, Cost>], threads: usize, link: f64) -> usize {
    let times: Vec<f64> = costs
        .iter()
        .map(|by| {
            let (a, b, c) = stage_secs(&by[class], class_total, threads, link);
            a.max(b).max(c)
        })
        .collect();
    let best = times.iter().cloned().fold(f64::INFINITY, f64::min);
    times.iter().position(|&t| t <= best * (1.0 + TIE_MARGIN)).unwrap()
}

fn print_class_table(
    classes: &[(String, f64)],
    by_class: &HashMap<String, Vec<&Entry>>,
    costs: &[HashMap<String, Cost>],
    total: u64,
    threads: usize,
    z1: usize,
) {
    let lz4 = 1;
    let z3 = CODECS.iter().position(|c| *c == Codec::Zstd(3)).unwrap();
    let z9 = CODECS.iter().position(|c| *c == Codec::Zstd(9)).unwrap();
    println!("\nclass        files     GB   share | ratio: lz4  zstd1  zstd3  zstd9 | best@100M  best@1G  best@2.5G  best@10G");
    for (class, bytes) in classes {
        let r = |i: usize| {
            let c = &costs[i][class];
            c.out / c.raw
        };
        print!(
            "{:<10} {:>7} {:>6.1} {:>6.1}% |       {:.2}  {:.2}   {:.2}   {:.2}  |",
            class,
            by_class[class].len(),
            bytes / 1e9,
            bytes / total as f64 * 100.0,
            r(lz4),
            r(z1),
            r(z3),
            r(z9)
        );
        for (_, link) in LINKS {
            print!(" {:>9}", CODECS[pick(class, *bytes, costs, threads, link)].name());
        }
        println!();
    }
}

/// Whole-folder time per strategy: each stage's work sums across classes, and the
/// pipeline runs at the pace of its slowest stage.
fn print_strategies(classes: &[(String, f64)], costs: &[HashMap<String, Cost>], total: u64, threads: usize) {
    let fixed = [Codec::None, Codec::Lz4, Codec::Zstd(-1), Codec::Zstd(1), Codec::Zstd(3), Codec::Zstd(9)];
    print!("\nestimated transfer time for {:.1} GB ({threads} threads each side)\nstrategy  ", total as f64 / 1e9);
    for (name, _) in LINKS {
        print!(" {:>16}", name);
    }
    println!();

    let run = |choose: &dyn Fn(&str, f64, f64) -> usize, link: f64| -> (f64, &'static str) {
        let (mut c, mut w, mut d) = (0.0, 0.0, 0.0);
        for (class, bytes) in classes {
            let i = choose(class, *bytes, link);
            let (a, b, e) = stage_secs(&costs[i][class], *bytes, threads, link);
            c += a;
            w += b;
            d += e;
        }
        let secs = c.max(w).max(d);
        let bottleneck = if secs == w {
            "net"
        } else if secs == c {
            "cpu-s"
        } else {
            "cpu-r"
        };
        (secs, bottleneck)
    };
    let fmt = |secs: f64, bn: &str| format!("{} {:>5}", fmt_time(secs), bn);

    let none_times: Vec<f64> = LINKS.iter().map(|(_, l)| run(&|_, _, _| 0, *l).0).collect();
    for codec in fixed {
        let i = CODECS.iter().position(|c| *c == codec).unwrap();
        print!("{:<10}", codec.name());
        for (_, link) in LINKS {
            let (s, bn) = run(&|_, _, _| i, link);
            print!(" {:>16}", fmt(s, bn));
        }
        println!();
    }
    print!("{:<10}", "adaptive");
    for (_, link) in LINKS {
        let (s, bn) = run(&|cl, b, l| pick(cl, b, costs, threads, l), link);
        print!(" {:>16}", fmt(s, bn));
    }
    println!();
    print!("{:<10}", "  vs none");
    for (k, (_, link)) in LINKS.iter().enumerate() {
        let (s, _) = run(&|cl, b, l| pick(cl, b, costs, threads, l), *link);
        print!(" {:>16}", format!("{:+.1}%", (s / none_times[k] - 1.0) * 100.0));
    }
    println!();
}

fn fmt_time(secs: f64) -> String {
    let s = secs.round() as u64;
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, s % 3600 / 60)
    } else {
        format!("{}m{:02}s", s / 60, s % 60)
    }
}

/// Creates many small files to measure per-file create cost on the receiving disk
/// (this is where antivirus real-time scanning shows up).
fn write_test(dir: &Path, files: usize, size_kb: usize) {
    let root = dir.join(format!("yeet-write-test-{}", std::process::id()));
    fs::create_dir_all(&root).expect("create test dir");
    let buf: Vec<u8> = (0..size_kb * 1024).map(|i| (i * 31 % 251) as u8).collect();

    for threads in [1, 8] {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
        let sub = root.join(format!("t{threads}"));
        fs::create_dir_all(&sub).unwrap();
        let t = Instant::now();
        pool.install(|| {
            (0..files).into_par_iter().for_each(|i| {
                let mut f = File::create(sub.join(format!("f{i:06}.dat"))).unwrap();
                f.write_all(&buf).unwrap();
            })
        });
        let secs = t.elapsed().as_secs_f64();
        println!(
            "write-test {} threads={threads}: {files} x {size_kb} KB in {:.2}s = {:.0} files/s, {:.1} MB/s",
            dir.display(),
            secs,
            files as f64 / secs,
            (files * size_kb * 1024) as f64 / MB / secs
        );
    }
    fs::remove_dir_all(&root).ok();
}
