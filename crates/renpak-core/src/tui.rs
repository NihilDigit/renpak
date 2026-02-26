//! Interactive TUI for renpak — analyze, configure exclusions, build with live progress.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::prelude::*;
use ratatui::widgets::*;
use ratatui::DefaultTerminal;

use crate::pipeline::{self, BuildStats, ProgressReport, DEFAULT_SKIP_PREFIXES, IMAGE_EXTS};
use crate::rpa::RpaReader;

// --- Embedded runtime files ---

const RUNTIME_INIT_RPY: &str = include_str!("../../../python/runtime/renpak_init.rpy");
const RUNTIME_LOADER_PY: &str = include_str!("../../../python/runtime/renpak_loader.py");

// --- Data structures ---

struct DirInfo {
    prefix: String,
    image_count: u32,
    image_bytes: u64,
    excluded: bool,
}

#[derive(Clone)]
struct BuildProgress {
    done: u32,
    total: u32,
    orig_bytes: u64,
    comp_bytes: u64,
    current_file: String,
    phase: String,
    warnings: Vec<String>,
}

enum BuildMsg {
    PhaseStart { total: u32, msg: String },
    TaskDone { done: u32, total: u32, msg: String, orig: u64, comp: u64 },
    PhaseEnd { msg: String },
    Warning(String),
    Finished(Result<BuildStats, String>),
}

enum Phase {
    Analyze,
    Building,
    Done(Result<BuildStats, String>),
}

struct App {
    game_dir: PathBuf,
    rpa_path: PathBuf,
    rpa_size: u64,
    dirs: Vec<DirInfo>,
    total_images: u32,
    total_image_bytes: u64,
    total_other: u32,
    selected: usize,
    scroll_offset: usize,
    quality: i32,
    speed: i32,
    workers: usize,
    phase: Phase,
    progress: BuildProgress,
    build_rx: Option<mpsc::Receiver<BuildMsg>>,
    start_time: Instant,
    installed: bool,
    status_msg: Option<String>,
}

// --- Channel-based progress reporter for build thread ---

struct ChannelProgress {
    tx: mpsc::Sender<BuildMsg>,
}

impl ProgressReport for ChannelProgress {
    fn phase_start(&self, total: u32, msg: &str) {
        let _ = self.tx.send(BuildMsg::PhaseStart { total, msg: msg.to_string() });
    }
    fn task_done(&self, done: u32, total: u32, msg: &str, orig: u64, comp: u64) {
        let _ = self.tx.send(BuildMsg::TaskDone {
            done, total, msg: msg.to_string(), orig, comp,
        });
    }
    fn phase_end(&self, _total: u32, msg: &str, _orig: u64, _comp: u64) {
        let _ = self.tx.send(BuildMsg::PhaseEnd { msg: msg.to_string() });
    }
    fn warning(&self, msg: &str) {
        let _ = self.tx.send(BuildMsg::Warning(msg.to_string()));
    }
}

// --- Classify RPA entries into directory groups ---

fn classify_dirs(rpa_path: &Path) -> Result<(Vec<DirInfo>, u32, u64, u32), String> {
    let mut reader = RpaReader::open(rpa_path).map_err(|e| format!("open RPA: {e}"))?;
    let index = reader.read_index().map_err(|e| format!("read index: {e}"))?;

    let default_skip: Vec<String> = DEFAULT_SKIP_PREFIXES.iter().map(|s| s.to_string()).collect();

    // Aggregate by directory prefix (3 components for images/actor/X/, 2 otherwise)
    let mut dir_map: HashMap<String, (u32, u64)> = HashMap::new();
    let mut total_other = 0u32;

    for entry in index.values() {
        let lower = entry.name.to_ascii_lowercase();
        let is_img = IMAGE_EXTS.iter().any(|e| lower.ends_with(e));
        if !is_img {
            total_other += 1;
            continue;
        }
        let parts: Vec<&str> = entry.name.split('/').collect();
        let depth = if parts.len() >= 4 { 3 } else { parts.len().saturating_sub(1).max(1) };
        let prefix = parts[..depth].join("/") + "/";
        let e = dir_map.entry(prefix).or_insert((0, 0));
        e.0 += 1;
        e.1 += entry.length;
    }

    let mut dirs: Vec<DirInfo> = dir_map
        .into_iter()
        .map(|(prefix, (count, bytes))| {
            let excluded = default_skip.iter().any(|p| prefix.to_ascii_lowercase().starts_with(p.as_str()));
            DirInfo { prefix, image_count: count, image_bytes: bytes, excluded }
        })
        .collect();
    dirs.sort_by(|a, b| b.image_bytes.cmp(&a.image_bytes));

    let total_images: u32 = dirs.iter().map(|d| d.image_count).sum();
    let total_bytes: u64 = dirs.iter().map(|d| d.image_bytes).sum();

    Ok((dirs, total_images, total_bytes, total_other))
}

impl App {
    fn new(game_dir: &Path) -> Result<Self, String> {
        let game_sub = game_dir.join("game");
        let search_dir = if game_sub.is_dir() { &game_sub } else { game_dir };

        let rpa_path = std::fs::read_dir(search_dir)
            .map_err(|e| format!("read dir: {e}"))?
            .filter_map(|e| e.ok())
            .find(|e| e.path().extension().is_some_and(|ext| ext == "rpa"))
            .map(|e| e.path())
            .ok_or_else(|| format!("No .rpa files in {}", search_dir.display()))?;

        let rpa_size = std::fs::metadata(&rpa_path).map(|m| m.len()).unwrap_or(0);
        let (dirs, total_images, total_image_bytes, total_other) = classify_dirs(&rpa_path)?;
        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);

        Ok(App {
            game_dir: game_dir.to_path_buf(),
            rpa_path,
            rpa_size,
            dirs,
            total_images,
            total_image_bytes,
            total_other,
            selected: 0,
            scroll_offset: 0,
            quality: 60,
            speed: 8,
            workers,
            phase: Phase::Analyze,
            progress: BuildProgress {
                done: 0, total: 0, orig_bytes: 0, comp_bytes: 0,
                current_file: String::new(), phase: String::new(),
                warnings: Vec::new(),
            },
            build_rx: None,
            start_time: Instant::now(),
            installed: false,
            status_msg: None,
        })
    }

    fn encode_count(&self) -> (u32, u64) {
        let mut count = 0u32;
        let mut bytes = 0u64;
        for d in &self.dirs {
            if !d.excluded {
                count += d.image_count;
                bytes += d.image_bytes;
            }
        }
        (count, bytes)
    }

    fn excluded_prefixes(&self) -> Vec<String> {
        self.dirs.iter().filter(|d| d.excluded).map(|d| d.prefix.clone()).collect()
    }

    fn start_build(&mut self) {
        let (tx, rx) = mpsc::channel();
        self.build_rx = Some(rx);
        self.phase = Phase::Building;
        self.start_time = Instant::now();

        let rpa_path = self.rpa_path.clone();
        let game_dir = self.rpa_path.parent().unwrap().to_path_buf();
        let work_dir = game_dir.join(".renpak_work");
        let _ = std::fs::create_dir_all(&work_dir);
        let out_rpa = work_dir.join(rpa_path.file_name().unwrap());
        let quality = self.quality;
        let speed = self.speed;
        let workers = self.workers;
        let exclude = self.excluded_prefixes();

        thread::spawn(move || {
            let progress = ChannelProgress { tx: tx.clone() };
            let result = pipeline::build(&rpa_path, &out_rpa, quality, speed, workers, &exclude, &progress);
            let _ = tx.send(BuildMsg::Finished(result));
        });
    }

    fn install(&self) -> Result<(), String> {
        let game_dir = self.rpa_path.parent().unwrap();
        let backup_dir = game_dir.join(".renpak_backup");
        std::fs::create_dir_all(&backup_dir).map_err(|e| format!("mkdir backup: {e}"))?;

        let rpa_name = self.rpa_path.file_name().unwrap();
        let backup = backup_dir.join(rpa_name);
        let work_rpa = game_dir.join(".renpak_work").join(rpa_name);

        // Verify build output exists before touching anything
        if !work_rpa.exists() {
            return Err(format!("Build output not found: {}", work_rpa.display()));
        }

        // Backup original only if no backup exists yet (preserve the true original)
        if !backup.exists() {
            std::fs::rename(&self.rpa_path, &backup)
                .map_err(|e| format!("backup: {e}"))?;
        }

        // Atomically replace the RPA (rename overwrites target on Linux)
        std::fs::rename(&work_rpa, &self.rpa_path)
            .map_err(|e| format!("install rpa: {e}"))?;
        let _ = std::fs::remove_dir(game_dir.join(".renpak_work"));

        // Write embedded runtime files
        std::fs::write(game_dir.join("renpak_init.rpy"), RUNTIME_INIT_RPY)
            .map_err(|e| format!("write init.rpy: {e}"))?;
        std::fs::write(game_dir.join("renpak_loader.py"), RUNTIME_LOADER_PY)
            .map_err(|e| format!("write loader.py: {e}"))?;

        Ok(())
    }

    fn launch_game(&self) -> Result<(), String> {
        // Find launch script in game root
        let entries = std::fs::read_dir(&self.game_dir)
            .map_err(|e| format!("read game dir: {e}"))?;
        let launcher = entries
            .filter_map(|e| e.ok())
            .find(|e| {
                let name = e.file_name();
                let n = name.to_string_lossy();
                n.ends_with(".sh") || n.ends_with(".exe") || n.ends_with(".py")
            })
            .map(|e| e.path())
            .ok_or_else(|| "No launcher found (.sh/.exe/.py)".to_string())?;

        std::process::Command::new(&launcher)
            .current_dir(&self.game_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("launch {}: {e}", launcher.display()))?;
        Ok(())
    }

    fn revert(&self) -> Result<(), String> {
        let game_dir = self.rpa_path.parent().unwrap();
        let backup = game_dir.join(".renpak_backup").join(self.rpa_path.file_name().unwrap());
        if !backup.exists() {
            return Err("No backup found".into());
        }
        // Restore backup → original path (atomic replace)
        std::fs::rename(&backup, &self.rpa_path)
            .map_err(|e| format!("revert: {e}"))?;
        // Remove runtime files
        let _ = std::fs::remove_file(game_dir.join("renpak_init.rpy"));
        let _ = std::fs::remove_file(game_dir.join("renpak_loader.py"));
        let _ = std::fs::remove_dir(game_dir.join(".renpak_backup"));
        Ok(())
    }

    fn delete_backup(&self) -> Result<(), String> {
        let game_dir = self.rpa_path.parent().unwrap();
        let backup_dir = game_dir.join(".renpak_backup");
        std::fs::remove_dir_all(&backup_dir)
            .map_err(|e| format!("delete backup: {e}"))?;
        Ok(())
    }

    fn poll_build(&mut self) {
        let rx = match &self.build_rx {
            Some(rx) => rx,
            None => return,
        };
        while let Ok(msg) = rx.try_recv() {
            match msg {
                BuildMsg::PhaseStart { total, msg } => {
                    self.progress.total = total;
                    self.progress.done = 0;
                    self.progress.phase = msg;
                }
                BuildMsg::TaskDone { done, total, msg, orig, comp } => {
                    self.progress.done = done;
                    self.progress.total = total;
                    self.progress.orig_bytes = orig;
                    self.progress.comp_bytes = comp;
                    self.progress.current_file = msg;
                }
                BuildMsg::PhaseEnd { msg } => {
                    self.progress.phase = msg;
                }
                BuildMsg::Warning(msg) => {
                    self.progress.warnings.push(msg);
                }
                BuildMsg::Finished(result) => {
                    self.phase = Phase::Done(result);
                    self.build_rx = None;
                    return;
                }
            }
        }
    }

    fn handle_key(&mut self, code: KeyCode) {
        match &self.phase {
            Phase::Analyze => match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if self.selected > 0 { self.selected -= 1; }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if self.selected + 1 < self.dirs.len() { self.selected += 1; }
                }
                KeyCode::Char(' ') => {
                    if let Some(d) = self.dirs.get_mut(self.selected) {
                        d.excluded = !d.excluded;
                    }
                }
                KeyCode::Char('+') | KeyCode::Char('=') => {
                    if self.quality < 63 { self.quality += 1; }
                }
                KeyCode::Char('-') => {
                    if self.quality > 1 { self.quality -= 1; }
                }
                KeyCode::Enter => {
                    self.start_build();
                }
                _ => {}
            },
            Phase::Done(_) => match code {
                KeyCode::Char('i') if !self.installed => {
                    match self.install() {
                        Ok(()) => {
                            self.installed = true;
                            self.status_msg = Some("Installed successfully".into());
                        }
                        Err(e) => {
                            self.status_msg = Some(format!("Install failed: {e}"));
                        }
                    }
                }
                KeyCode::Char('l') if self.installed => {
                    match self.launch_game() {
                        Ok(()) => self.status_msg = Some("Game launched".into()),
                        Err(e) => self.status_msg = Some(format!("Launch failed: {e}")),
                    }
                }
                KeyCode::Char('r') if self.installed => {
                    match self.revert() {
                        Ok(()) => {
                            self.installed = false;
                            self.status_msg = Some("Reverted to original".into());
                        }
                        Err(e) => self.status_msg = Some(format!("Revert failed: {e}")),
                    }
                }
                KeyCode::Char('d') if self.installed => {
                    match self.delete_backup() {
                        Ok(()) => self.status_msg = Some("Backup deleted".into()),
                        Err(e) => self.status_msg = Some(format!("Delete failed: {e}")),
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn draw(&self, frame: &mut Frame) {
        match &self.phase {
            Phase::Analyze => self.draw_analyze(frame),
            Phase::Building => self.draw_building(frame),
            Phase::Done(result) => self.draw_done(frame, result),
        }
    }

    fn draw_analyze(&self, frame: &mut Frame) {
        let area = frame.area();
        let rpa_name = self.rpa_path.file_name().unwrap().to_string_lossy();
        let rpa_mb = self.rpa_size as f64 / 1_048_576.0;

        let layout = Layout::vertical([
            Constraint::Length(3),  // header
            Constraint::Min(8),    // directory list
            Constraint::Length(3), // stats
            Constraint::Length(3), // controls
        ]).split(area);

        // Header
        let header = Paragraph::new(Line::from(vec![
            " renpak ".bold(),
            "— ".dark_gray(),
            rpa_name.to_string().into(),
            format!(" ({:.0} MB)", rpa_mb).dark_gray(),
        ])).block(Block::bordered().border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(header, layout[0]);

        // Directory list
        let visible_height = layout[1].height.saturating_sub(2) as usize;
        // Adjust scroll to keep selected visible
        let scroll = if self.selected < self.scroll_offset {
            self.selected
        } else if self.selected >= self.scroll_offset + visible_height {
            self.selected - visible_height + 1
        } else {
            self.scroll_offset
        };

        let items: Vec<ListItem> = self.dirs.iter().enumerate()
            .skip(scroll)
            .take(visible_height)
            .map(|(i, d)| {
                let marker = if d.excluded { "✗" } else { "✓" };
                let marker_style = if d.excluded { Style::default().fg(Color::Red) } else { Style::default().fg(Color::Green) };
                let sel = if i == self.selected { "▸ " } else { "  " };
                let mb = d.image_bytes as f64 / 1_048_576.0;
                ListItem::new(Line::from(vec![
                    sel.into(),
                    Span::styled(marker, marker_style),
                    " ".into(),
                    Span::styled(format!("{:<40}", d.prefix), if i == self.selected { Style::default().bold() } else { Style::default() }),
                    format!("{:>5}", d.image_count).dark_gray(),
                    format!("{:>8.0} MB", mb).into(),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(Block::bordered().title(" Directories ").border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(list, layout[1]);

        // Stats
        let (enc_count, enc_bytes) = self.encode_count();
        let _skip_count = self.total_images - enc_count;
        let stats = Paragraph::new(Line::from(vec![
            format!(" Encode: {} images ({:.0} MB)", enc_count, enc_bytes as f64 / 1_048_576.0).green(),
            "  ".into(),
            format!("Skip: {} dirs", self.dirs.iter().filter(|d| d.excluded).count()).red(),
            "  ".into(),
            format!("Quality: {}", self.quality).yellow(),
            format!("  Workers: {}", self.workers).dark_gray(),
        ])).block(Block::bordered().border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(stats, layout[2]);

        // Controls
        let controls = Paragraph::new(Line::from(vec![
            " ↑↓".blue().bold(), " Navigate ".dark_gray(),
            "Space".blue().bold(), " Toggle ".dark_gray(),
            "+/-".blue().bold(), " Quality ".dark_gray(),
            "Enter".blue().bold(), " Start ".dark_gray(),
            "Esc".blue().bold(), " Quit".dark_gray(),
        ]));
        frame.render_widget(controls, layout[3]);
    }

    fn draw_building(&self, frame: &mut Frame) {
        let area = frame.area();
        let p = &self.progress;
        let elapsed = self.start_time.elapsed().as_secs_f64();

        let layout = Layout::vertical([
            Constraint::Length(3),  // header
            Constraint::Length(3),  // progress bar
            Constraint::Length(7),  // stats
            Constraint::Length(3),  // current file
            Constraint::Min(0),    // warnings
        ]).split(area);

        // Header
        let header = Paragraph::new(Line::from(vec![
            " renpak ".bold(),
            "— ".dark_gray(),
            "Building".yellow(),
        ])).block(Block::bordered().border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(header, layout[0]);

        // Progress bar
        let ratio = if p.total > 0 { p.done as f64 / p.total as f64 } else { 0.0 };
        let gauge = LineGauge::default()
            .filled_style(Style::default().fg(Color::Cyan))
            .unfilled_style(Style::default().fg(Color::DarkGray))
            .label(format!("  {}/{}  {:.0}%", p.done, p.total, ratio * 100.0))
            .ratio(ratio)
            .block(Block::bordered().border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(gauge, layout[1]);

        // Stats
        let eta = if p.done > 0 {
            let rate = p.done as f64 / elapsed;
            (p.total - p.done) as f64 / rate
        } else { 0.0 };
        let orig_mb = p.orig_bytes as f64 / 1_048_576.0;
        let comp_mb = p.comp_bytes as f64 / 1_048_576.0;
        let pct = if orig_mb > 0.0 { comp_mb / orig_mb * 100.0 } else { 0.0 };
        let rate = if elapsed > 0.0 { p.done as f64 / elapsed } else { 0.0 };

        let stats = Paragraph::new(vec![
            Line::from(vec![
                format!("  Elapsed: {}", fmt_duration(elapsed)).into(),
                format!("    ETA: {}", fmt_duration(eta)).dark_gray(),
                format!("    {:.1} img/s", rate).into(),
            ]),
            Line::from(""),
            Line::from(vec![
                format!("  Original:   {:.0} MB", orig_mb).into(),
            ]),
            Line::from(vec![
                format!("  Compressed: {:.0} MB", comp_mb).green(),
                format!(" ({:.0}%)", pct).dark_gray(),
            ]),
            Line::from(vec![
                format!("  Workers: {}", self.workers).dark_gray(),
            ]),
        ]).block(Block::bordered().border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(stats, layout[2]);

        // Current file
        let current = Paragraph::new(Line::from(vec![
            "  ".into(),
            Span::styled(&p.current_file, Style::default().fg(Color::DarkGray)),
        ])).block(Block::bordered().title(" Current ").border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(current, layout[3]);

        // Warnings
        if !p.warnings.is_empty() {
            let warns: Vec<ListItem> = p.warnings.iter().rev().take(5)
                .map(|w| ListItem::new(Span::styled(w.as_str(), Style::default().fg(Color::Yellow))))
                .collect();
            let warn_list = List::new(warns)
                .block(Block::bordered().title(" Warnings ").border_style(Style::default().fg(Color::Yellow)));
            frame.render_widget(warn_list, layout[4]);
        }
    }

    fn draw_done(&self, frame: &mut Frame, result: &Result<BuildStats, String>) {
        let area = frame.area();
        let elapsed = self.start_time.elapsed().as_secs_f64();

        let layout = Layout::vertical([
            Constraint::Length(3),  // header
            Constraint::Min(8),    // stats
            Constraint::Length(1), // status message
            Constraint::Length(3), // controls
        ]).split(area);

        match result {
            Ok(stats) => {
                let header_text = if self.installed { "Installed" } else { "Done" };
                let header = Paragraph::new(Line::from(vec![
                    " renpak ".bold(),
                    "— ".dark_gray(),
                    Span::styled(format!("{header_text} "), Style::default().fg(Color::Green)),
                    "✓".green().bold(),
                ])).block(Block::bordered().border_style(Style::default().fg(Color::Green)));
                frame.render_widget(header, layout[0]);

                let rpa_mb = self.rpa_size as f64 / 1_048_576.0;
                let out_size = std::fs::metadata(
                    self.rpa_path.parent().unwrap().join(".renpak_work")
                        .join(self.rpa_path.file_name().unwrap())
                ).map(|m| m.len()).unwrap_or(0);
                let out_mb = if out_size > 0 {
                    out_size as f64 / 1_048_576.0
                } else {
                    std::fs::metadata(&self.rpa_path).map(|m| m.len() as f64 / 1_048_576.0).unwrap_or(0.0)
                };
                let orig_mb = stats.original_bytes as f64 / 1_048_576.0;
                let comp_mb = stats.compressed_bytes as f64 / 1_048_576.0;

                let body = Paragraph::new(vec![
                    Line::from(""),
                    Line::from(vec![
                        format!("  RPA:    {:.0} MB → {:.0} MB", rpa_mb, out_mb).into(),
                        format!(" ({:.0}%)", out_mb / rpa_mb * 100.0).dark_gray(),
                    ]),
                    Line::from(vec![
                        format!("  Images: {:.0} MB → {:.0} MB", orig_mb, comp_mb).green(),
                        format!(" ({:.0}%)", comp_mb / orig_mb * 100.0).dark_gray(),
                    ]),
                    Line::from(""),
                    Line::from(format!("  Encoded: {}  Passthrough: {}  Errors: {}",
                        stats.encoded, stats.passthrough, stats.encode_errors)),
                    Line::from(format!("  Time: {}", fmt_duration(elapsed))),
                ]).block(Block::bordered().border_style(Style::default().fg(Color::DarkGray)));
                frame.render_widget(body, layout[1]);

                // Status message
                if let Some(msg) = &self.status_msg {
                    let style = if msg.contains("failed") || msg.contains("Failed") {
                        Style::default().fg(Color::Red)
                    } else {
                        Style::default().fg(Color::Yellow)
                    };
                    frame.render_widget(Paragraph::new(Span::styled(format!("  {msg}"), style)), layout[2]);
                }

                // Controls based on install state
                let controls = if self.installed {
                    Paragraph::new(Line::from(vec![
                        " l".blue().bold(), " Launch game ".dark_gray(),
                        "r".blue().bold(), " Revert ".dark_gray(),
                        "d".blue().bold(), " Delete backup ".dark_gray(),
                        "q".blue().bold(), " Quit".dark_gray(),
                    ]))
                } else {
                    Paragraph::new(Line::from(vec![
                        " i".blue().bold(), " Install ".dark_gray(),
                        "q".blue().bold(), " Quit".dark_gray(),
                    ]))
                };
                frame.render_widget(controls, layout[3]);
            }
            Err(msg) => {
                let header = Paragraph::new(Line::from(vec![
                    " renpak ".bold(),
                    "— ".dark_gray(),
                    "Error ".red(),
                    "✗".red().bold(),
                ])).block(Block::bordered().border_style(Style::default().fg(Color::Red)));
                frame.render_widget(header, layout[0]);

                let body = Paragraph::new(vec![
                    Line::from(""),
                    Line::from(format!("  {msg}").red()),
                ]).block(Block::bordered().border_style(Style::default().fg(Color::DarkGray)));
                frame.render_widget(body, layout[1]);

                let controls = Paragraph::new(Line::from(vec![
                    " q".blue().bold(), " Quit".dark_gray(),
                ]));
                frame.render_widget(controls, layout[3]);
            }
        }
    }
}

fn fmt_duration(secs: f64) -> String {
    let s = secs as u64;
    if s >= 60 { format!("{}m {:02}s", s / 60, s % 60) } else { format!("{s}s") }
}

/// Public entry point: launch TUI for a game directory.
pub fn run(game_dir: &Path) -> Result<(), String> {
    let mut app = App::new(game_dir)?;

    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal, &mut app);
    ratatui::restore();

    result.map_err(|e| format!("TUI error: {e}"))
}

fn run_loop(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    loop {
        terminal.draw(|frame| app.draw(frame))?;

        // Poll build messages if building
        if matches!(app.phase, Phase::Building) {
            app.poll_build();
        }

        let timeout = if matches!(app.phase, Phase::Building) {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(200)
        };

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press { continue; }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        if !matches!(app.phase, Phase::Building) {
                            return Ok(());
                        }
                    }
                    code => app.handle_key(code),
                }
            }
        }
    }
}
