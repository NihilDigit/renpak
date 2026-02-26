use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use renpak_core::pipeline::{self, ProgressReport};

// --- CLI progress reporter ---

struct CliProgress {
    start: Instant,
    phase_start: AtomicU64, // nanos since start, for per-phase ETA
}

impl CliProgress {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            phase_start: AtomicU64::new(0),
        }
    }
    fn elapsed(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }
    fn phase_elapsed(&self) -> f64 {
        let ps = self.phase_start.load(Ordering::Relaxed);
        self.start.elapsed().as_secs_f64() - (ps as f64 / 1e9)
    }
}

impl ProgressReport for CliProgress {
    fn phase_start(&self, total: u32, msg: &str) {
        let ns = self.start.elapsed().as_nanos() as u64;
        self.phase_start.store(ns, Ordering::Relaxed);
        eprintln!("[{:7.1}s] === {} ===", self.elapsed(), msg);
        let _ = total;
    }

    fn task_done(&self, done: u32, total: u32, msg: &str, orig: u64, comp: u64) {
        let pct = if total > 0 { done as f64 / total as f64 * 100.0 } else { 0.0 };
        let pe = self.phase_elapsed();
        let eta = if done > 0 {
            let rate = done as f64 / pe;
            (total - done) as f64 / rate
        } else { 0.0 };

        if orig > 0 {
            let orig_mb = orig as f64 / 1_048_576.0;
            let comp_mb = comp as f64 / 1_048_576.0;
            let ratio = if orig_mb > 0.0 { comp_mb / orig_mb * 100.0 } else { 0.0 };
            eprintln!("  [{:7.1}s] {}/{} ({:.0}%) {:.0}MB->{:.0}MB ({:.0}%) ETA {:.0}s  {}",
                self.elapsed(), done, total, pct, orig_mb, comp_mb, ratio, eta, msg);
        } else {
            eprintln!("  [{:7.1}s] {}/{} ({:.0}%)  {}",
                self.elapsed(), done, total, pct, msg);
        }
    }

    fn phase_end(&self, _total: u32, msg: &str, orig: u64, comp: u64) {
        if orig > 0 {
            let orig_mb = orig as f64 / 1_048_576.0;
            let comp_mb = comp as f64 / 1_048_576.0;
            eprintln!("[{:7.1}s] === {} ({:.0}MB -> {:.0}MB) ===",
                self.elapsed(), msg, orig_mb, comp_mb);
        } else {
            eprintln!("[{:7.1}s] === {} ===", self.elapsed(), msg);
        }
    }

    fn warning(&self, msg: &str) {
        eprintln!("  [{:7.1}s] WARN: {}", self.elapsed(), msg);
    }
}

// --- Argument parsing ---

struct Args {
    input: PathBuf,
    output: PathBuf,
    quality: i32,
    speed: i32,
    workers: usize,
    exclude: Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        return Err(format!(
            "Usage: {} <input.rpa> <output.rpa> [-q quality] [-s speed] [-j workers] [-x prefix]...",
            args[0]
        ));
    }

    let mut a = Args {
        input: PathBuf::from(&args[1]),
        output: PathBuf::from(&args[2]),
        quality: 60,
        speed: 8,
        workers: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4),
        exclude: Vec::new(),
    };

    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "-q" | "--quality" => { i += 1; a.quality = args[i].parse().map_err(|e| format!("-q: {e}"))?; }
            "-s" | "--speed"   => { i += 1; a.speed = args[i].parse().map_err(|e| format!("-s: {e}"))?; }
            "-j" | "--workers" => { i += 1; a.workers = args[i].parse().map_err(|e| format!("-j: {e}"))?; }
            "-x" | "--exclude" => { i += 1; a.exclude.push(args[i].clone()); }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    Ok(a)
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => { eprintln!("{msg}"); std::process::exit(1); }
    };

    eprintln!("renpak build");
    eprintln!("  input:   {}", args.input.display());
    eprintln!("  output:  {}", args.output.display());
    eprintln!("  quality: {}, speed: {}, workers: {}", args.quality, args.speed, args.workers);
    if !args.exclude.is_empty() {
        eprintln!("  exclude: {:?}", args.exclude);
    }
    eprintln!();

    // Ensure output directory exists
    if let Some(parent) = args.output.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let progress = CliProgress::new();
    match pipeline::build(&args.input, &args.output, args.quality, args.speed, args.workers, &args.exclude, &progress) {
        Ok(stats) => {
            let in_mb = std::fs::metadata(&args.input).map(|m| m.len() as f64 / 1_048_576.0).unwrap_or(0.0);
            let out_mb = std::fs::metadata(&args.output).map(|m| m.len() as f64 / 1_048_576.0).unwrap_or(0.0);
            eprintln!();
            eprintln!("Done in {:.0}s", progress.elapsed());
            eprintln!("  RPA: {:.0}MB -> {:.0}MB ({:.0}%)", in_mb, out_mb, out_mb / in_mb * 100.0);
            eprintln!("  Images: {:.0}MB -> {:.0}MB ({:.0}%)",
                stats.original_bytes as f64 / 1_048_576.0,
                stats.compressed_bytes as f64 / 1_048_576.0,
                stats.compressed_bytes as f64 / stats.original_bytes as f64 * 100.0);
            eprintln!("  {} encoded, {} passthrough, {} errors",
                stats.encoded, stats.passthrough, stats.encode_errors);
        }
        Err(msg) => {
            eprintln!("FATAL: {msg}");
            std::process::exit(1);
        }
    }
}
