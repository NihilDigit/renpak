use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use renpak_core::pipeline::{self, ProgressReport, Vp9BuildOptions, Vp9PlanMode};

// --- CLI progress reporter ---

struct CliProgress {
    start: Instant,
    phase_start: AtomicU64,
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
        let pct = if total > 0 {
            done as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        let pe = self.phase_elapsed();
        let eta = if done > 0 {
            (total - done) as f64 / (done as f64 / pe)
        } else {
            0.0
        };
        if orig > 0 {
            let (om, cm) = (orig as f64 / 1_048_576.0, comp as f64 / 1_048_576.0);
            eprintln!(
                "  [{:7.1}s] {}/{} ({:.0}%) {:.0}MB->{:.0}MB ({:.0}%) ETA {:.0}s  {}",
                self.elapsed(),
                done,
                total,
                pct,
                om,
                cm,
                cm / om * 100.0,
                eta,
                msg
            );
        } else {
            eprintln!(
                "  [{:7.1}s] {}/{} ({:.0}%)  {}",
                self.elapsed(),
                done,
                total,
                pct,
                msg
            );
        }
    }
    fn phase_end(&self, _total: u32, msg: &str, orig: u64, comp: u64) {
        if orig > 0 {
            let (om, cm) = (orig as f64 / 1_048_576.0, comp as f64 / 1_048_576.0);
            eprintln!(
                "[{:7.1}s] === {} ({:.0}MB -> {:.0}MB) ===",
                self.elapsed(),
                msg,
                om,
                cm
            );
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
    BuildVp9 {
        input: PathBuf,
        output: PathBuf,
        options: Vp9BuildOptions,
        work_dir: PathBuf,
        android_assets_dir: Option<PathBuf>,
        exclude: Vec<String>,
        sample_mode: bool,
    },
    PlanVp9 {
        input: PathBuf,
        options: Vp9BuildOptions,
        exclude: Vec<String>,
        json: bool,
        exact: bool,
    },
    DoctorAndroid {
        renpy_sdk: Option<PathBuf>,
        rapt_root: Option<PathBuf>,
    },
}

fn usage() {
    eprintln!("renpak -- AVIF compressor for Ren'Py games");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  renpak                                     TUI (current directory)");
    eprintln!("  renpak <game_dir>                          TUI (specified directory)");
    eprintln!("  renpak build <in.rpa> <out.rpa> [options]  Headless build");
    eprintln!("  renpak build-vp9 <in.rpa> <out.rpa> [options]");
    eprintln!("  renpak build-vp9-sample <in.rpa> <out.rpa> [options]");
    eprintln!("  renpak plan-vp9 <in.rpa> [options]         Scan VP9 candidates without encoding");
    eprintln!("  renpak doctor android [options]            Check Android/RAPT toolchain");
    eprintln!();
    eprintln!("Build options:");
    eprintln!("  -p, --preset <P>    Quality preset: high, medium, low (default: medium)");
    eprintln!("  -q, --quality <N>   AVIF quality 0-100 (overrides preset)");
    eprintln!("  -s, --speed <N>     Encoder speed 0-10 (overrides preset)");
    eprintln!("  -w, --workers <N>   Worker threads (default: auto)");
    eprintln!("  -x, --exclude <P>   Exclude prefix (repeatable)");
    eprintln!(
        "  --limit <N>         VP9 frame count limit (default: all for build-vp9, 8 for sample)"
    );
    eprintln!("  --no-image          Do not bundle image assets");
    eprintln!("  --video-limit <N>   VP9 video count limit (default: all, sample disables video)");
    eprintln!("  --no-video          Do not re-encode video assets");
    eprintln!("  --bundle-size <N>   Max frames per VP9 bundle, 1-32 (default: 16)");
    eprintln!("  --min-width <N>     Minimum VP9 candidate width (default: 640)");
    eprintln!("  --min-height <N>    Minimum VP9 candidate height (default: 360)");
    eprintln!("  --min-savings-percent <N>");
    eprintln!("                       Keep a VP9 bundle only if it saves at least N% (default: 8)");
    eprintln!("  --allow-transparent Include transparent images in VP9 candidate selection");
    eprintln!("  --strip-optimized  Remove original files replaced by accepted VP9 bundle frames");
    eprintln!("  --work-dir <DIR>    Scratch directory for VP9 sample encoding");
    eprintln!("  --android-assets-dir <DIR>");
    eprintln!("                       Also write manifest and VP9 bundles as loose Android assets");
    eprintln!("  --json              Machine-readable output for plan-vp9");
    eprintln!(
        "  --exact             Decode candidate images during plan-vp9 to check transparency"
    );
    eprintln!("Doctor Android options:");
    eprintln!("  --renpy-sdk <DIR>    Ren'Py SDK root");
    eprintln!("  --rapt-root <DIR>    RAPT root, usually <renpy-sdk>/rapt");
}

fn parse_args() -> Result<Command, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();

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
        let mut workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let mut exclude = Vec::new();
        let mut i = 3;
        while i < args.len() {
            match args[i].as_str() {
                "-p" | "--preset" => {
                    i += 1;
                    match args.get(i).map(|s| s.as_str()) {
                        Some("high") => {
                            quality = 75;
                            speed = 6;
                        }
                        Some("medium") => {
                            quality = 60;
                            speed = 8;
                        }
                        Some("low") => {
                            quality = 40;
                            speed = 10;
                        }
                        Some(o) => {
                            return Err(format!("Unknown preset: {o} (use high/medium/low)"))
                        }
                        None => return Err("--preset requires a value".into()),
                    }
                }
                "-q" | "--quality" => {
                    i += 1;
                    quality = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(60);
                }
                "-s" | "--speed" => {
                    i += 1;
                    speed = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(8);
                }
                "-w" | "--workers" => {
                    i += 1;
                    workers = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(workers);
                }
                "-x" | "--exclude" => {
                    i += 1;
                    if let Some(p) = args.get(i) {
                        exclude.push(p.clone());
                    }
                }
                other => return Err(format!("Unknown option: {other}")),
            }
            i += 1;
        }
        Ok(Command::Build {
            input,
            output,
            quality,
            speed,
            workers,
            exclude,
        })
    } else if args[0] == "doctor" {
        if args.get(1).map(|s| s.as_str()) != Some("android") {
            usage();
            return Err("doctor requires the android target".into());
        }
        let mut renpy_sdk = None;
        let mut rapt_root = None;
        let mut i = 2;
        while i < args.len() {
            match args[i].as_str() {
                "--renpy-sdk" => {
                    i += 1;
                    if let Some(p) = args.get(i) {
                        renpy_sdk = Some(PathBuf::from(p));
                    } else {
                        return Err("--renpy-sdk requires a value".into());
                    }
                }
                "--rapt-root" => {
                    i += 1;
                    if let Some(p) = args.get(i) {
                        rapt_root = Some(PathBuf::from(p));
                    } else {
                        return Err("--rapt-root requires a value".into());
                    }
                }
                other => return Err(format!("Unknown option: {other}")),
            }
            i += 1;
        }
        Ok(Command::DoctorAndroid {
            renpy_sdk,
            rapt_root,
        })
    } else if args[0] == "build-vp9" || args[0] == "build-vp9-sample" || args[0] == "plan-vp9" {
        let plan_mode = args[0] == "plan-vp9";
        if (!plan_mode && args.len() < 3) || (plan_mode && args.len() < 2) {
            usage();
            return Err(if plan_mode {
                "plan-vp9 requires <input>".to_string()
            } else {
                format!("{} requires <input> <output>", args[0])
            });
        }
        let sample_mode = args[0] == "build-vp9-sample";
        let input = PathBuf::from(&args[1]);
        let output = if plan_mode {
            PathBuf::new()
        } else {
            PathBuf::from(&args[2])
        };
        let mut options = if sample_mode {
            Vp9BuildOptions {
                limit: 8,
                video_limit: 0,
                min_width: 1,
                min_height: 1,
                skip_transparent: false,
                optimize_videos: false,
                ..Vp9BuildOptions::default()
            }
        } else {
            Vp9BuildOptions::default()
        };
        let mut work_dir = if plan_mode {
            PathBuf::from(".")
        } else {
            output
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(".renpak-work")
        };
        let mut android_assets_dir = None;
        let mut exclude = Vec::new();
        let mut json = false;
        let mut exact = false;
        let mut i = if plan_mode { 2 } else { 3 };
        while i < args.len() {
            match args[i].as_str() {
                "--limit" => {
                    i += 1;
                    options.limit = args
                        .get(i)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(options.limit);
                }
                "--no-image" => {
                    options.limit = 0;
                }
                "--bundle-size" => {
                    i += 1;
                    options.bundle_size = args
                        .get(i)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(options.bundle_size);
                }
                "--video-limit" => {
                    i += 1;
                    options.video_limit = args
                        .get(i)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(options.video_limit);
                }
                "--no-video" => {
                    options.optimize_videos = false;
                }
                "--min-width" => {
                    i += 1;
                    options.min_width = args
                        .get(i)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(options.min_width);
                }
                "--min-height" => {
                    i += 1;
                    options.min_height = args
                        .get(i)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(options.min_height);
                }
                "--min-savings-percent" => {
                    i += 1;
                    options.min_savings_percent = args
                        .get(i)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(options.min_savings_percent);
                }
                "--allow-transparent" => {
                    options.skip_transparent = false;
                }
                "--strip-optimized" => {
                    options.strip_optimized = true;
                }
                "--work-dir" => {
                    i += 1;
                    if let Some(p) = args.get(i) {
                        work_dir = PathBuf::from(p);
                    }
                }
                "--android-assets-dir" => {
                    i += 1;
                    if let Some(p) = args.get(i) {
                        android_assets_dir = Some(PathBuf::from(p));
                    } else {
                        return Err("--android-assets-dir requires a value".into());
                    }
                }
                "--json" => {
                    json = true;
                }
                "--exact" => {
                    exact = true;
                }
                "-x" | "--exclude" => {
                    i += 1;
                    if let Some(p) = args.get(i) {
                        exclude.push(p.clone());
                    }
                }
                other => return Err(format!("Unknown option: {other}")),
            }
            i += 1;
        }
        if plan_mode {
            Ok(Command::PlanVp9 {
                input,
                options,
                exclude,
                json,
                exact,
            })
        } else {
            Ok(Command::BuildVp9 {
                input,
                output,
                options,
                work_dir,
                android_assets_dir,
                exclude,
                sample_mode,
            })
        }
    } else {
        Ok(Command::Tui(PathBuf::from(&args[0])))
    }
}

// --- Headless build ---

fn command_line(program: &str, args: &[&str]) -> Result<String, String> {
    let output = ProcessCommand::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if output.status.success() {
        Ok(text.trim().to_string())
    } else {
        Err(text.trim().to_string())
    }
}

fn command_line_path(program: &Path, args: &[&str]) -> Result<String, String> {
    let output = ProcessCommand::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{}: {e}", program.display()))?;
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if output.status.success() {
        Ok(text.trim().to_string())
    } else {
        Err(text.trim().to_string())
    }
}

fn doctor_ok(label: &str, msg: &str) {
    println!("[OK]   {label}: {msg}");
}

fn doctor_warn(label: &str, msg: &str) {
    println!("[WARN] {label}: {msg}");
}

fn doctor_err(label: &str, msg: &str) {
    println!("[ERR]  {label}: {msg}");
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

fn run_doctor_android(renpy_sdk: Option<&Path>, rapt_root: Option<&Path>) {
    let mut errors = 0u32;

    match command_line("ffmpeg", &["-version"]) {
        Ok(out) => doctor_ok("ffmpeg", first_line(&out)),
        Err(msg) => {
            errors += 1;
            doctor_err("ffmpeg", &msg);
        }
    }

    let java_home = std::env::var("JAVA_HOME").ok().map(PathBuf::from);
    let java_bin = java_home
        .as_ref()
        .map(|home| home.join("bin/java"))
        .filter(|path| path.is_file());
    let java_result = match java_bin.as_ref() {
        Some(path) => command_line_path(path, &["-version"]),
        None => command_line("java", &["-version"]),
    };
    match java_result {
        Ok(out) => doctor_ok("java", first_line(&out)),
        Err(msg) => {
            errors += 1;
            doctor_err("java", &msg);
        }
    }

    match std::env::var("ANDROID_HOME") {
        Ok(home) if Path::new(&home).join("platform-tools/adb").is_file() => {
            doctor_ok("ANDROID_HOME", &home)
        }
        Ok(home) => {
            errors += 1;
            doctor_err(
                "ANDROID_HOME",
                &format!("{home} (platform-tools/adb missing)"),
            );
        }
        Err(_) => {
            errors += 1;
            doctor_err("ANDROID_HOME", "not set");
        }
    }

    match command_line("adb", &["devices", "-l"]) {
        Ok(out) => {
            let devices: Vec<&str> = out
                .lines()
                .skip(1)
                .filter(|line| line.contains(" device "))
                .collect();
            if devices.is_empty() {
                doctor_warn("adb device", "no online Android device");
            } else {
                doctor_ok("adb device", devices[0].trim());
            }
        }
        Err(msg) => {
            errors += 1;
            doctor_err("adb", &msg);
        }
    }

    let codec_result = command_line("adb", &["shell", "cmd", "media.codec", "list"])
        .or_else(|_| command_line("adb", &["shell", "dumpsys", "media.player"]));
    match codec_result {
        Ok(out) => {
            let vp9_line = out
                .lines()
                .find(|line| line.to_ascii_lowercase().contains("vp9"));
            if let Some(line) = vp9_line {
                doctor_ok("device VP9 codec", line.trim());
            } else {
                doctor_warn(
                    "device VP9 codec",
                    "no VP9 codec reported by media.codec list",
                );
            }
        }
        Err(msg) => doctor_warn("device VP9 codec", &msg),
    }

    if let Some(sdk) = renpy_sdk {
        let renpy_sh = sdk.join("renpy.sh");
        if renpy_sh.is_file() {
            doctor_ok("Ren'Py SDK", &sdk.display().to_string());
        } else {
            errors += 1;
            doctor_err("Ren'Py SDK", &format!("missing {}", renpy_sh.display()));
        }
    } else {
        doctor_warn(
            "Ren'Py SDK",
            "not provided; pass --renpy-sdk for APK build checks",
        );
    }

    let inferred_rapt = renpy_sdk.map(|sdk| sdk.join("rapt"));
    let rapt = rapt_root.or(inferred_rapt.as_deref());
    if let Some(root) = rapt {
        let activity = root
            .join("prototype/renpyandroid/src/main/java/org/renpy/android/PythonSDLActivity.java");
        if activity.is_file() {
            doctor_ok("RAPT", &root.display().to_string());
        } else {
            errors += 1;
            doctor_err("RAPT", &format!("missing {}", activity.display()));
        }
    } else {
        doctor_warn("RAPT", "not provided; pass --rapt-root or --renpy-sdk");
    }

    if errors > 0 {
        std::process::exit(1);
    }
}

fn run_headless(
    input: &Path,
    output: &Path,
    quality: i32,
    speed: i32,
    workers: usize,
    exclude: &[String],
) {
    let cancel = AtomicBool::new(false);
    let progress = CliProgress::new();
    match pipeline::build(
        input, output, quality, speed, workers, exclude, &progress, &cancel, None,
    ) {
        Ok(stats) if stats.cancelled => {
            eprintln!("\nCancelled.");
            std::process::exit(130);
        }
        Ok(stats) => {
            let orig_mb = stats.original_bytes as f64 / 1_048_576.0;
            let comp_mb = stats.compressed_bytes as f64 / 1_048_576.0;
            eprintln!(
                "\nDone: {} encoded, {} passthrough, {} errors{}",
                stats.encoded,
                stats.passthrough,
                stats.encode_errors,
                if stats.cache_hits > 0 {
                    format!(", {} cached", stats.cache_hits)
                } else {
                    String::new()
                }
            );
            eprintln!(
                "Images: {:.0} MB -> {:.0} MB ({:.0}%)",
                orig_mb,
                comp_mb,
                if orig_mb > 0.0 {
                    comp_mb / orig_mb * 100.0
                } else {
                    0.0
                }
            );
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

fn print_vp9_plan(stats: &pipeline::Vp9PlanStats) {
    let selected_mb = stats.selected_original_bytes as f64 / 1_048_576.0;
    println!("VP9 plan");
    println!("  mode: {:?}", stats.mode);
    println!("  entries: {}", stats.total_entries);
    println!(
        "  images: {} (excluded by prefix: {}, inspected: {})",
        stats.image_entries, stats.excluded_image_entries, stats.inspected_image_entries
    );
    println!(
        "  selected: {} frames, {:.1} MiB",
        stats.selected_frames, selected_mb
    );
    println!(
        "  planned bundles before size guard: {}",
        stats.planned_bundles_before_size_guard
    );
    println!(
        "  grouped-by-dimension bundle floor: {}",
        stats.planned_bundles_grouped_by_dimension
    );
    println!(
        "  skipped: small={}, transparent={}, transparency_unchecked={}, decode_errors={}, read_errors={}",
        stats.skipped_small,
        stats.skipped_transparent,
        stats.transparency_unchecked,
        stats.decode_errors,
        stats.read_errors
    );
    println!("  limit reached: {}", stats.limit_reached);
    println!("  top dimensions:");
    for dim in stats.dimensions.iter().take(10) {
        println!(
            "    {}x{}: {} frames, {:.1} MiB, {} bundles",
            dim.width,
            dim.height,
            dim.frames,
            dim.original_bytes as f64 / 1_048_576.0,
            dim.planned_bundles
        );
    }
}

fn run_vp9_plan(
    input: &Path,
    options: &Vp9BuildOptions,
    exclude: &[String],
    json: bool,
    exact: bool,
) {
    let cancel = AtomicBool::new(false);
    let progress = CliProgress::new();
    let mode = if exact {
        Vp9PlanMode::Exact
    } else {
        Vp9PlanMode::Fast
    };
    let result = pipeline::plan_vp9_with_mode(input, options, exclude, mode, &progress, &cancel);
    match result {
        Ok(stats) => {
            if json {
                match serde_json::to_string_pretty(&stats) {
                    Ok(text) => println!("{text}"),
                    Err(e) => {
                        eprintln!("Error: serialize VP9 plan: {e}");
                        std::process::exit(1);
                    }
                }
            } else {
                print_vp9_plan(&stats);
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

fn run_vp9_sample(
    input: &Path,
    output: &Path,
    options: &Vp9BuildOptions,
    work_dir: &Path,
    android_assets_dir: Option<&Path>,
    exclude: &[String],
    sample_mode: bool,
) {
    let cancel = AtomicBool::new(false);
    let progress = CliProgress::new();
    let result = if sample_mode {
        pipeline::build_vp9_sample(
            input,
            output,
            options.limit,
            options.bundle_size,
            work_dir,
            android_assets_dir,
            exclude,
            &progress,
            &cancel,
        )
    } else {
        pipeline::build_vp9(
            input,
            output,
            options,
            work_dir,
            android_assets_dir,
            exclude,
            &progress,
            &cancel,
        )
    };
    match result {
        Ok(stats) if stats.cancelled => {
            eprintln!("\nCancelled.");
            std::process::exit(130);
        }
        Ok(stats) => {
            eprintln!(
                "\nDone: VP9 mobile profile with {} optimized assets, {} passthrough entries",
                stats.encoded, stats.passthrough
            );
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
        Ok(Command::Build {
            input,
            output,
            quality,
            speed,
            workers,
            exclude,
        }) => {
            run_headless(&input, &output, quality, speed, workers, &exclude);
        }
        Ok(Command::BuildVp9 {
            input,
            output,
            options,
            work_dir,
            android_assets_dir,
            exclude,
            sample_mode,
        }) => {
            run_vp9_sample(
                &input,
                &output,
                &options,
                &work_dir,
                android_assets_dir.as_deref(),
                &exclude,
                sample_mode,
            );
        }
        Ok(Command::PlanVp9 {
            input,
            options,
            exclude,
            json,
            exact,
        }) => {
            run_vp9_plan(&input, &options, &exclude, json, exact);
        }
        Ok(Command::DoctorAndroid {
            renpy_sdk,
            rapt_root,
        }) => {
            run_doctor_android(renpy_sdk.as_deref(), rapt_root.as_deref());
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}
