use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use renpak_core::pipeline::{self, ProgressReport};

// --- CLI progress reporter ---

struct CliProgress {
    start: Instant,
    phase_start: AtomicU64,
}

impl CliProgress {
    fn new() -> Self {
        Self { start: Instant::now(), phase_start: AtomicU64::new(0) }
    }
    fn elapsed(&self) -> f64 { self.start.elapsed().as_secs_f64() }
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
        let eta = if done > 0 { (total - done) as f64 / (done as f64 / pe) } else { 0.0 };
        if orig > 0 {
            let (om, cm) = (orig as f64 / 1_048_576.0, comp as f64 / 1_048_576.0);
            eprintln!("  [{:7.1}s] {}/{} ({:.0}%) {:.0}MB->{:.0}MB ({:.0}%) ETA {:.0}s  {}",
                self.elapsed(), done, total, pct, om, cm, cm/om*100.0, eta, msg);
        } else {
            eprintln!("  [{:7.1}s] {}/{} ({:.0}%)  {}", self.elapsed(), done, total, pct, msg);
        }
    }
    fn phase_end(&self, _total: u32, msg: &str, orig: u64, comp: u64) {
        if orig > 0 {
            let (om, cm) = (orig as f64 / 1_048_576.0, comp as f64 / 1_048_576.0);
            eprintln!("[{:7.1}s] === {} ({:.0}MB -> {:.0}MB) ===", self.elapsed(), msg, om, cm);
        } else {
            eprintln!("[{:7.1}s] === {} ===", self.elapsed(), msg);
        }
    }
    fn warning(&self, msg: &str) {
        eprintln!("  [WARN] {}", msg);
    }
}

// --- CLI argument parsing ---

enum Command {
    Tui(PathBuf),
    Build {
        input: PathBuf,
        output: PathBuf,
        quality: i32,
        speed: i32,
        workers: usize,
        exclude: Vec<String>,
    },
}

fn usage() {
    eprintln!("renpak — AVIF compressor for Ren'Py games");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  renpak                                     TUI (current directory)");
    eprintln!("  renpak <game_dir>                          TUI (specified directory)");
    eprintln!("  renpak build <in.rpa> <out.rpa> [options]  Headless build");
    eprintln!();
    eprintln!("Build options:");
    eprintln!("  -q, --quality <N>   AVIF quality 0-63 (default: 60)");
    eprintln!("  -s, --speed <N>     Encoder speed 0-10 (default: 8)");
    eprintln!("  -w, --workers <N>   Worker threads (default: auto)");
    eprintln!("  -x, --exclude <P>   Exclude prefix (repeatable)");
}

fn parse_args() -> Result<Command, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // No args or --help
    if args.is_empty() {
        let cwd = std::env::current_dir().map_err(|e| format!("current dir: {e}"))?;
        return Ok(Command::Tui(cwd));
    }
    if args[0] == "-h" || args[0] == "--help" {
        usage();
        std::process::exit(0);
    }

    if args[0] == "build" {
        if args.len() < 3 {
            usage();
            return Err("build requires <input> <output>".into());
        }
        let input = PathBuf::from(&args[1]);
        let output = PathBuf::from(&args[2]);
        let mut quality = 60;
        let mut speed = 8;
        let mut workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let mut exclude = Vec::new();
        let mut i = 3;
        while i < args.len() {
            match args[i].as_str() {
                "-q" | "--quality" => { i += 1; quality = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(60); }
                "-s" | "--speed" => { i += 1; speed = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(8); }
                "-w" | "--workers" => { i += 1; workers = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(workers); }
                "-x" | "--exclude" => { i += 1; if let Some(p) = args.get(i) { exclude.push(p.clone()); } }
                other => return Err(format!("Unknown option: {other}")),
            }
            i += 1;
        }
        Ok(Command::Build { input, output, quality, speed, workers, exclude })
    } else {
        Ok(Command::Tui(PathBuf::from(&args[0])))
    }
}

// --- Headless build ---

fn run_headless(input: &Path, output: &Path, quality: i32, speed: i32, workers: usize, exclude: &[String]) {
    let progress = CliProgress::new();
    match pipeline::build(input, output, quality, speed, workers, exclude, &progress) {
        Ok(stats) => {
            let orig_mb = stats.original_bytes as f64 / 1_048_576.0;
            let comp_mb = stats.compressed_bytes as f64 / 1_048_576.0;
            eprintln!("\nDone: {} encoded, {} passthrough, {} errors",
                stats.encoded, stats.passthrough, stats.encode_errors);
            eprintln!("Images: {:.0} MB -> {:.0} MB ({:.0}%)", orig_mb, comp_mb,
                if orig_mb > 0.0 { comp_mb / orig_mb * 100.0 } else { 0.0 });
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

fn main() {
    match parse_args() {
        Ok(Command::Tui(game_dir)) => {
            if let Err(e) = renpak_core::tui::run(&game_dir) {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
        Ok(Command::Build { input, output, quality, speed, workers, exclude }) => {
            run_headless(&input, &output, quality, speed, workers, &exclude);
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}
