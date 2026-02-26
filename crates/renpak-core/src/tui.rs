//! Interactive TUI for renpak — analyze, configure, build with live progress.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::pipeline::{self, BuildStats, ProgressReport, DEFAULT_SKIP_PREFIXES, IMAGE_EXTS};
use crate::rpa::RpaReader;

// --- Embedded runtime files ---

const RUNTIME_INIT_RPY: &str = include_str!("../../../python/runtime/renpak_init.rpy");
const RUNTIME_LOADER_PY: &str = include_str!("../../../python/runtime/renpak_loader.py");

// --- Quality presets ---

#[derive(Clone, Copy, PartialEq)]
pub enum QualityPreset {
    High,
    Medium,
    Low,
}

impl QualityPreset {
    pub fn quality(self) -> i32 {
        match self { Self::High => 75, Self::Medium => 60, Self::Low => 40 }
    }
    pub fn speed(self) -> i32 {
        match self { Self::High => 6, Self::Medium => 8, Self::Low => 10 }
    }
    fn label(self) -> &'static str {
        match self { Self::High => "High", Self::Medium => "Medium", Self::Low => "Low" }
    }
    fn desc(self) -> &'static str {
        match self {
            Self::High => "Best quality, slower",
            Self::Medium => "Balanced",
            Self::Low => "Smallest, fastest",
        }
    }
    fn next(self) -> Self {
        match self { Self::High => Self::Medium, Self::Medium => Self::Low, Self::Low => Self::Low }
    }
    fn prev(self) -> Self {
        match self { Self::High => Self::High, Self::Medium => Self::High, Self::Low => Self::Medium }
    }
}
// --- Data structures ---

struct DirInfo {
    prefix: String,       // full path like "images/bg/"
    display_name: String, // segment like "bg/"
    depth: usize,
    own_count: u32,       // images directly in this prefix
    own_bytes: u64,
    subtree_count: u32,   // total including descendants
    subtree_bytes: u64,
    excluded: bool,
    has_children: bool,
    expanded: bool,
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

// --- Click regions for mouse support ---

#[derive(Default)]
struct ClickRegions {
    preset_high: Option<Rect>,
    preset_medium: Option<Rect>,
    preset_low: Option<Rect>,
    perf_low: Option<Rect>,
    perf_medium: Option<Rect>,
    perf_high: Option<Rect>,
    start_btn: Option<Rect>,
    dir_list_area: Option<Rect>,
    dir_list_scroll: usize,
    // Done screen buttons
    install_btn: Option<Rect>,
    launch_btn: Option<Rect>,
    revert_btn: Option<Rect>,
    delete_btn: Option<Rect>,
    quit_btn: Option<Rect>,
}

struct App {
    game_dir: PathBuf,
    rpa_path: PathBuf,
    rpa_size: u64,
    dirs: Vec<DirInfo>,
    visible: Vec<usize>,  // indices into dirs (visible after expand/collapse)
    selected: usize,
    scroll_offset: usize,
    dir_visible_h: usize,
    preset: QualityPreset,
    workers: usize,
    max_workers: usize,
    phase: Phase,
    progress: BuildProgress,
    build_rx: Option<mpsc::Receiver<BuildMsg>>,
    start_time: Instant,
    installed: bool,
    status_msg: Option<String>,
    cancel_flag: Arc<AtomicBool>,
    cancelling: bool,
    has_cache: bool,
    click: RefCell<ClickRegions>,
    focus: usize,      // Tab-cycling: 0=Directories, 1=Quality, 2=Performance, 3=Actions
    action_idx: usize, // Left/Right within Actions block
    wants_quit: bool,
    already_compressed: bool, // RPA already contains AVIF files
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

fn classify_dirs(rpa_path: &Path) -> Result<(Vec<DirInfo>, u32, u64, u32, u32), String> {
    let mut reader = RpaReader::open(rpa_path).map_err(|e| format!("open RPA: {e}"))?;
    let index = reader.read_index().map_err(|e| format!("read index: {e}"))?;

    let default_skip: Vec<String> = DEFAULT_SKIP_PREFIXES.iter().map(|s| s.to_string()).collect();
    let mut dir_map: HashMap<String, (u32, u64)> = HashMap::new();
    let mut total_other = 0u32;
    let mut total_avif = 0u32;

    for entry in index.values() {
        let lower = entry.name.to_ascii_lowercase();
        if lower.ends_with(".avif") {
            total_avif += 1;
            total_other += 1;
            continue;
        }
        let is_img = IMAGE_EXTS.iter().any(|e| lower.ends_with(e));
        if !is_img {
            total_other += 1;
            continue;
        }
        // Use immediate parent directory as prefix
        if let Some(pos) = entry.name.rfind('/') {
            let prefix = format!("{}/", &entry.name[..pos]);
            let e = dir_map.entry(prefix).or_insert((0, 0));
            e.0 += 1;
            e.1 += entry.length;
        } else {
            let e = dir_map.entry("./".to_string()).or_insert((0, 0));
            e.0 += 1;
            e.1 += entry.length;
        }
    }

    // Build tree from flat prefixes
    struct Node {
        children: HashMap<String, Node>,
        own_count: u32,
        own_bytes: u64,
    }
    impl Node {
        fn new() -> Self { Node { children: HashMap::new(), own_count: 0, own_bytes: 0 } }
        fn subtree_count(&self) -> u32 {
            self.own_count + self.children.values().map(|c| c.subtree_count()).sum::<u32>()
        }
        fn subtree_bytes(&self) -> u64 {
            self.own_bytes + self.children.values().map(|c| c.subtree_bytes()).sum::<u64>()
        }
    }

    let mut root = Node::new();
    for (prefix, &(count, bytes)) in &dir_map {
        let segments: Vec<&str> = prefix.trim_end_matches('/').split('/').collect();
        let mut node = &mut root;
        for seg in segments {
            node = node.children.entry(seg.to_string()).or_insert_with(Node::new);
        }
        node.own_count += count;
        node.own_bytes += bytes;
    }

    // Flatten tree: excluded first at each level, then by subtree size desc
    fn flatten(
        node: &Node, parent_path: &str, depth: usize,
        skip: &[String], out: &mut Vec<DirInfo>,
    ) {
        let mut children: Vec<(&String, &Node)> = node.children.iter().collect();
        children.sort_by(|a, b| {
            let ap = format!("{}{}/", parent_path, a.0);
            let bp = format!("{}{}/", parent_path, b.0);
            let ae = skip.iter().any(|p| ap.to_ascii_lowercase().starts_with(p));
            let be = skip.iter().any(|p| bp.to_ascii_lowercase().starts_with(p));
            ae.cmp(&be).reverse().then(b.1.subtree_bytes().cmp(&a.1.subtree_bytes()))
        });

        for (seg, child) in children {
            let full = format!("{}{}/", parent_path, seg);
            let excluded = skip.iter().any(|p| full.to_ascii_lowercase().starts_with(p));
            let has_children = !child.children.is_empty();
            out.push(DirInfo {
                prefix: full.clone(),
                display_name: format!("{}/", seg),
                depth,
                own_count: child.own_count,
                own_bytes: child.own_bytes,
                subtree_count: child.subtree_count(),
                subtree_bytes: child.subtree_bytes(),
                excluded,
                has_children,
                expanded: depth == 0 && has_children,
            });
            flatten(child, &full, depth + 1, skip, out);
        }
    }

    let mut dirs = Vec::new();
    flatten(&root, "", 0, &default_skip, &mut dirs);

    let total_images: u32 = dirs.iter().map(|d| d.own_count).sum();
    let total_bytes: u64 = dirs.iter().map(|d| d.own_bytes).sum();

    Ok((dirs, total_images, total_bytes, total_other, total_avif))
}
// --- App implementation ---

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
        let (dirs, _total_images, _, _, total_avif) = classify_dirs(&rpa_path)?;
        let already_compressed = total_avif > 0;
        let max_workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let workers = max_workers; // default: High (all cores)

        let game_sub_dir = rpa_path.parent().unwrap().to_path_buf();
        let cache_dir = game_sub_dir.join(".renpak_work/cache");
        let has_cache = cache_dir.is_dir()
            && std::fs::read_dir(&cache_dir).map(|mut d| d.next().is_some()).unwrap_or(false);

        let visible: Vec<usize> = (0..dirs.len()).collect(); // will be refreshed below

        let mut app = App {
            game_dir: game_dir.to_path_buf(), rpa_path, rpa_size, dirs, visible,
            selected: 0, scroll_offset: 0, dir_visible_h: 10, preset: QualityPreset::Medium, workers, max_workers,
            phase: Phase::Analyze,
            progress: BuildProgress {
                done: 0, total: 0, orig_bytes: 0, comp_bytes: 0,
                current_file: String::new(), phase: String::new(), warnings: Vec::new(),
            },
            build_rx: None, start_time: Instant::now(),
            installed: false, status_msg: None,
            cancel_flag: Arc::new(AtomicBool::new(false)), cancelling: false,
            has_cache, click: RefCell::new(ClickRegions::default()),
            focus: 0, action_idx: 0, wants_quit: false,
            already_compressed,
        };
        app.refresh_visible();

        // Already compressed → skip straight to Done (installed state)
        if already_compressed {
            let backup_exists = game_sub_dir.join(".renpak_backup")
                .join(app.rpa_path.file_name().unwrap()).exists();
            app.phase = Phase::Done(Ok(pipeline::BuildStats {
                total_entries: 0, encoded: total_avif as u32,
                passthrough: 0, original_bytes: 0, compressed_bytes: 0,
                encode_errors: 0, cache_hits: 0, cancelled: false,
                timing: pipeline::BuildTiming::default(),
            }));
            app.installed = true;
            if !backup_exists {
                app.status_msg = Some("No backup found -- revert unavailable".into());
            }
        }

        Ok(app)
    }

    fn refresh_visible(&mut self) {
        self.visible.clear();
        let mut skip_below: Option<usize> = None;
        for (i, d) in self.dirs.iter().enumerate() {
            if let Some(depth) = skip_below {
                if d.depth <= depth { skip_below = None; } else { continue; }
            }
            self.visible.push(i);
            if d.has_children && !d.expanded {
                skip_below = Some(d.depth);
            }
        }
        if !self.visible.contains(&self.selected) {
            self.selected = self.visible.iter().rev()
                .find(|&&idx| idx <= self.selected).copied()
                .unwrap_or(self.visible.first().copied().unwrap_or(0));
        }
    }

    fn vis_pos(&self) -> Option<usize> {
        self.visible.iter().position(|&i| i == self.selected)
    }

    fn encode_count(&self) -> (u32, u64) {
        let (mut c, mut b) = (0u32, 0u64);
        for d in &self.dirs { if !d.excluded { c += d.own_count; b += d.own_bytes; } }
        (c, b)
    }

    fn excluded_prefixes(&self) -> Vec<String> {
        self.dirs.iter().filter(|d| d.excluded).map(|d| d.prefix.clone()).collect()
    }

    fn start_build(&mut self) {
        if self.already_compressed {
            self.status_msg = Some("RPA already compressed. Revert to original first.".into());
            return;
        }
        // Pre-flight: check disk space
        let game_dir = self.rpa_path.parent().unwrap();
        match fs2::available_space(game_dir) {
            Ok(avail) => {
                if avail < self.rpa_size {
                    self.status_msg = Some(format!(
                        "Not enough disk space: {:.0} MB free, need {:.0} MB",
                        avail as f64 / 1_048_576.0,
                        self.rpa_size as f64 / 1_048_576.0,
                    ));
                    return;
                }
            }
            Err(_) => {} // can't check, proceed anyway
        }
        let (tx, rx) = mpsc::channel();
        self.build_rx = Some(rx);
        self.phase = Phase::Building;
        self.start_time = Instant::now();
        self.cancel_flag.store(false, Ordering::Relaxed);
        self.cancelling = false;

        let rpa_path = self.rpa_path.clone();
        let game_dir = self.rpa_path.parent().unwrap().to_path_buf();
        let work_dir = game_dir.join(".renpak_work");
        let cache_dir = work_dir.join("cache");
        let _ = std::fs::create_dir_all(&cache_dir);
        let out_rpa = work_dir.join(rpa_path.file_name().unwrap());
        let quality = self.preset.quality();
        let speed = self.preset.speed();
        let workers = self.workers;
        let exclude = self.excluded_prefixes();
        let cancel = self.cancel_flag.clone();

        thread::spawn(move || {
            let progress = ChannelProgress { tx: tx.clone() };
            let result = pipeline::build(
                &rpa_path, &out_rpa, quality, speed, workers,
                &exclude, &progress, &cancel, Some(&cache_dir),
            );
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
        if !work_rpa.exists() {
            return Err(format!("Build output not found: {}", work_rpa.display()));
        }
        if !backup.exists() {
            std::fs::rename(&self.rpa_path, &backup).map_err(|e| format!("backup: {e}"))?;
        }
        std::fs::rename(&work_rpa, &self.rpa_path).map_err(|e| format!("install rpa: {e}"))?;
        // Clean up work dir (including cache)
        let _ = std::fs::remove_dir_all(game_dir.join(".renpak_work"));
        // Write runtime files
        std::fs::write(game_dir.join("renpak_init.rpy"), RUNTIME_INIT_RPY)
            .map_err(|e| format!("write init.rpy: {e}"))?;
        std::fs::write(game_dir.join("renpak_loader.py"), RUNTIME_LOADER_PY)
            .map_err(|e| format!("write loader.py: {e}"))?;
        Ok(())
    }

    fn launch_game(&self) -> Result<(), String> {
        let entries: Vec<_> = std::fs::read_dir(&self.game_dir)
            .map_err(|e| format!("read dir: {e}"))?
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name(); let n = n.to_string_lossy();
                n.ends_with(".sh") || n.ends_with(".exe") || n.ends_with(".py")
            })
            .map(|e| e.path())
            .collect();

        // Platform-aware priority: .sh first on unix, .exe first on windows
        #[cfg(unix)]
        let priority = [".sh", ".py", ".exe"];
        #[cfg(windows)]
        let priority = [".exe", ".py", ".sh"];

        let launcher = priority.iter()
            .find_map(|ext| entries.iter().find(|p| p.to_string_lossy().ends_with(ext)))
            .ok_or_else(|| "No launcher found (.sh/.exe/.py)".to_string())?;

        std::process::Command::new(launcher)
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
        if !backup.exists() { return Err("No backup found".into()); }
        std::fs::rename(&backup, &self.rpa_path).map_err(|e| format!("revert: {e}"))?;
        let _ = std::fs::remove_file(game_dir.join("renpak_init.rpy"));
        let _ = std::fs::remove_file(game_dir.join("renpak_loader.py"));
        let _ = std::fs::remove_dir(game_dir.join(".renpak_backup"));
        Ok(())
    }

    fn delete_backup(&self) -> Result<(), String> {
        let game_dir = self.rpa_path.parent().unwrap();
        std::fs::remove_dir_all(game_dir.join(".renpak_backup"))
            .map_err(|e| format!("delete backup: {e}"))?;
        Ok(())
    }

    fn poll_build(&mut self) {
        let rx = match &self.build_rx { Some(rx) => rx, None => return };
        while let Ok(msg) = rx.try_recv() {
            match msg {
                BuildMsg::PhaseStart { total, msg } => {
                    self.progress.total = total; self.progress.done = 0; self.progress.phase = msg;
                }
                BuildMsg::TaskDone { done, total, msg, orig, comp } => {
                    self.progress.done = done; self.progress.total = total;
                    self.progress.orig_bytes = orig; self.progress.comp_bytes = comp;
                    self.progress.current_file = msg;
                }
                BuildMsg::PhaseEnd { msg } => { self.progress.phase = msg; }
                BuildMsg::Warning(msg) => { self.progress.warnings.push(msg); }
                BuildMsg::Finished(result) => {
                    self.phase = Phase::Done(result);
                    self.build_rx = None;
                    self.focus = 0;
                    self.action_idx = 0;
                    return;
                }
            }
        }
    }
    fn focus_count(&self) -> usize {
        match &self.phase {
            Phase::Analyze => 4, // Directories, Quality, Performance, Actions
            Phase::Building => 0,
            Phase::Done(_) => 1, // Actions only
        }
    }

    fn action_count(&self) -> usize {
        match &self.phase {
            Phase::Analyze => 2, // Start, Quit
            Phase::Done(result) => {
                let cancelled = matches!(result, Ok(s) if s.cancelled);
                if cancelled { 2 } // Resume, Quit
                else if self.installed { 4 } // Launch, Revert, Delete, Quit
                else { 2 } // Install, Quit
            }
            Phase::Building => 0,
        }
    }

    fn focus_next(&mut self) {
        let n = self.focus_count();
        if n > 0 { self.focus = (self.focus + 1) % n; self.action_idx = 0; }
    }

    fn focus_prev(&mut self) {
        let n = self.focus_count();
        if n > 0 { self.focus = (self.focus + n - 1) % n; self.action_idx = 0; }
    }

    fn worker_tiers(&self) -> [usize; 3] {
        let m = self.max_workers;
        [(m / 4).max(1), m / 2, m]
    }

    fn workers_up(&mut self) {
        let tiers = self.worker_tiers();
        for &t in &tiers {
            if t > self.workers { self.workers = t; return; }
        }
    }

    fn workers_down(&mut self) {
        let tiers = self.worker_tiers();
        for &t in tiers.iter().rev() {
            if t < self.workers { self.workers = t; return; }
        }
    }

    fn worker_tier_label(&self) -> &'static str {
        let tiers = self.worker_tiers();
        if self.workers <= tiers[0] { "Low" }
        else if self.workers <= tiers[1] { "Medium" }
        else { "High" }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        // Tab / Shift+Tab: cycle focus in any non-building phase
        if code == KeyCode::Tab || code == KeyCode::BackTab {
            if modifiers.contains(KeyModifiers::SHIFT) || code == KeyCode::BackTab {
                self.focus_prev();
            } else {
                self.focus_next();
            }
            return;
        }

        match &self.phase {
            Phase::Analyze => match self.focus {
                0 => match code { // Directories block
                    KeyCode::Up | KeyCode::Char('k') => {
                        if let Some(pos) = self.vis_pos() {
                            if pos > 0 { self.selected = self.visible[pos - 1]; }
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if let Some(pos) = self.vis_pos() {
                            if pos + 1 < self.visible.len() { self.selected = self.visible[pos + 1]; }
                        }
                    }
                    KeyCode::Char(' ') => {
                        if self.selected < self.dirs.len() {
                            let new_state = !self.dirs[self.selected].excluded;
                            let parent = self.dirs[self.selected].prefix.clone();
                            for d in &mut self.dirs {
                                if d.prefix.starts_with(&parent) { d.excluded = new_state; }
                            }
                        }
                    }
                    KeyCode::Enter => {
                        if self.selected < self.dirs.len() && self.dirs[self.selected].has_children {
                            self.dirs[self.selected].expanded = !self.dirs[self.selected].expanded;
                            self.refresh_visible();
                        }
                    }
                    _ => {}
                },
                1 => match code { // Quality block
                    KeyCode::Left | KeyCode::Char('h') => { self.preset = self.preset.prev(); }
                    KeyCode::Right | KeyCode::Char('l') => { self.preset = self.preset.next(); }
                    _ => {}
                },
                2 => match code { // Performance block
                    KeyCode::Left | KeyCode::Char('h') => { self.workers_down(); }
                    KeyCode::Right | KeyCode::Char('l') => { self.workers_up(); }
                    _ => {}
                },
                3 => match code { // Actions block
                    KeyCode::Left | KeyCode::Char('h') => {
                        if self.action_idx > 0 { self.action_idx -= 1; }
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        let max = self.action_count().saturating_sub(1);
                        if self.action_idx < max { self.action_idx += 1; }
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        match self.action_idx {
                            0 => self.start_build(), // Start
                            _ => {} // Quit handled in run_loop
                        }
                    }
                    _ => {}
                },
                _ => {}
            },
            Phase::Done(result) => {
                let cancelled = matches!(result, Ok(s) if s.cancelled);
                match code {
                    KeyCode::Left | KeyCode::Char('h') => {
                        if self.action_idx > 0 { self.action_idx -= 1; }
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        let max = self.action_count().saturating_sub(1);
                        if self.action_idx < max { self.action_idx += 1; }
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        if cancelled {
                            match self.action_idx {
                                0 => self.start_build(), // Resume
                                _ => {} // Quit handled in run_loop
                            }
                        } else if self.installed {
                            match self.action_idx {
                                0 => self.handle_action('l'), // Launch
                                1 => self.handle_action('r'), // Revert
                                2 => self.handle_action('d'), // Delete
                                _ => {} // Quit handled in run_loop
                            }
                        } else {
                            match self.action_idx {
                                0 => self.handle_action('i'), // Install
                                _ => {} // Quit handled in run_loop
                            }
                        }
                    }
                    _ => {}
                }
            },
            _ => {}
        }
    }

    fn handle_action(&mut self, action: char) {
        match action {
            'i' => match self.install() {
                Ok(()) => { self.installed = true; self.action_idx = 0; self.status_msg = Some("Installed successfully".into()); }
                Err(e) => { self.status_msg = Some(format!("Install failed: {e}")); }
            },
            'l' => match self.launch_game() {
                Ok(()) => self.status_msg = Some("Game launched".into()),
                Err(e) => self.status_msg = Some(format!("Launch failed: {e}")),
            },
            'r' => match self.revert() {
                Ok(()) => { self.installed = false; self.action_idx = 0; self.status_msg = Some("Reverted to original".into()); }
                Err(e) => self.status_msg = Some(format!("Revert failed: {e}")),
            },
            'd' => match self.delete_backup() {
                Ok(()) => self.status_msg = Some("Backup deleted".into()),
                Err(e) => self.status_msg = Some(format!("Delete failed: {e}")),
            },
            _ => {}
        }
    }

    fn handle_mouse(&mut self, event: MouseEvent) {
        // Scroll wheel in directory list
        match event.kind {
            MouseEventKind::ScrollUp if matches!(self.phase, Phase::Analyze) => {
                let step = 3.min(self.scroll_offset);
                if step > 0 {
                    self.scroll_offset -= step;
                    // Clamp selected into visible viewport
                    let vis_end = self.scroll_offset + self.dir_visible_h;
                    if let Some(pos) = self.vis_pos() {
                        if pos >= vis_end {
                            self.selected = self.visible[vis_end.saturating_sub(1).min(self.visible.len() - 1)];
                        }
                    }
                }
                return;
            }
            MouseEventKind::ScrollDown if matches!(self.phase, Phase::Analyze) => {
                let max_scroll = self.visible.len().saturating_sub(self.dir_visible_h);
                let step = 3.min(max_scroll.saturating_sub(self.scroll_offset));
                if step > 0 {
                    self.scroll_offset += step;
                    if let Some(pos) = self.vis_pos() {
                        if pos < self.scroll_offset {
                            self.selected = self.visible[self.scroll_offset.min(self.visible.len() - 1)];
                        }
                    }
                }
                return;
            }
            MouseEventKind::Down(MouseButton::Left) => {}
            _ => return,
        }

        let (col, row) = (event.column, event.row);
        let cr = self.click.borrow();

        match &self.phase {
            Phase::Analyze => {
                if hit(cr.preset_high, col, row) { drop(cr); self.preset = QualityPreset::High; return; }
                if hit(cr.preset_medium, col, row) { drop(cr); self.preset = QualityPreset::Medium; return; }
                if hit(cr.preset_low, col, row) { drop(cr); self.preset = QualityPreset::Low; return; }
                if hit(cr.perf_low, col, row) { drop(cr); self.workers = self.worker_tiers()[0]; return; }
                if hit(cr.perf_medium, col, row) { drop(cr); self.workers = self.worker_tiers()[1]; return; }
                if hit(cr.perf_high, col, row) { drop(cr); self.workers = self.worker_tiers()[2]; return; }
                if hit(cr.start_btn, col, row) { drop(cr); self.start_build(); return; }
                if hit(cr.quit_btn, col, row) { drop(cr); self.wants_quit = true; return; }
                if let Some(area) = cr.dir_list_area {
                    if row > area.y && row < area.y + area.height - 1 && col > area.x && col < area.x + area.width - 1 {
                        let vis_idx = cr.dir_list_scroll + (row - area.y - 1) as usize;
                        drop(cr);
                        if vis_idx < self.visible.len() {
                            let dir_idx = self.visible[vis_idx];
                            let inner_right = area.x + area.width - 1;
                            let cb_start = inner_right.saturating_sub(4);
                            let d_depth = self.dirs[dir_idx].depth as u16;
                            let expand_start = area.x + 1 + 2 + d_depth * 2;
                            let expand_end = expand_start + 2;

                            if col >= cb_start {
                                // Checkbox → toggle exclude (cascade)
                                let new_state = !self.dirs[dir_idx].excluded;
                                let parent = self.dirs[dir_idx].prefix.clone();
                                for d in &mut self.dirs {
                                    if d.prefix.starts_with(&parent) { d.excluded = new_state; }
                                }
                            } else if self.dirs[dir_idx].has_children && col >= expand_start && col < expand_end {
                                // Expand marker → toggle expand/collapse
                                self.dirs[dir_idx].expanded = !self.dirs[dir_idx].expanded;
                                self.refresh_visible();
                            } else {
                                // Elsewhere → select
                                self.selected = dir_idx;
                            }
                        }
                        return;
                    }
                }
            }
            Phase::Done(result) => {
                let cancelled = matches!(result, Ok(s) if s.cancelled);
                if cancelled {
                    if hit(cr.start_btn, col, row) { drop(cr); self.start_build(); return; }
                } else if !self.installed {
                    if hit(cr.install_btn, col, row) { drop(cr); self.handle_action('i'); return; }
                } else {
                    if hit(cr.launch_btn, col, row) { drop(cr); self.handle_action('l'); return; }
                    if hit(cr.revert_btn, col, row) { drop(cr); self.handle_action('r'); return; }
                    if hit(cr.delete_btn, col, row) { drop(cr); self.handle_action('d'); return; }
                }
                if hit(cr.quit_btn, col, row) { drop(cr); self.wants_quit = true; return; }
            }
            Phase::Building => {
                // q/Esc to cancel is handled in run_loop
            }
        }
    }
    fn draw(&mut self, frame: &mut Frame) {
        self.click.borrow_mut().clear();
        if matches!(self.phase, Phase::Analyze) {
            self.draw_analyze(frame);
        } else if matches!(self.phase, Phase::Building) {
            self.draw_building(frame);
        } else {
            self.draw_done(frame);
        }
    }

    fn draw_analyze(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let rpa_name = self.rpa_path.file_name().unwrap().to_string_lossy();
        let rpa_mb = self.rpa_size as f64 / 1_048_576.0;

        let layout = Layout::vertical([
            Constraint::Length(3),  // header
            Constraint::Length(1),  // hint
            Constraint::Min(6),    // directory list (Block 0)
            Constraint::Length(3), // quality presets (Block 1)
            Constraint::Length(3), // performance (Block 2)
            Constraint::Length(4), // actions: stats + buttons (Block 3)
            Constraint::Length(1), // controls hint
        ]).split(area);

        // Header
        let mut header_spans = vec![
            " renpak ".bold(), "| ".dark_gray(),
            rpa_name.to_string().into(),
            format!(" ({:.0} MB)", rpa_mb).dark_gray(),
        ];
        if self.has_cache {
            header_spans.push("  [cache found]".yellow());
        }
        if self.already_compressed {
            header_spans.push("  [already compressed -- revert first]".red());
        }
        let header = Paragraph::new(Line::from(header_spans))
            .block(Block::bordered().border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(header, layout[0]);

        // Hint / status message
        if let Some(msg) = &self.status_msg {
            let style = if msg.contains("ailed") || msg.contains("already") { Style::default().fg(Color::Red) }
                else { Style::default().fg(Color::Yellow) };
            frame.render_widget(Paragraph::new(Span::styled(format!(" {msg}"), style)), layout[1]);
        } else {
            frame.render_widget(Paragraph::new(Line::from(vec![
                " Exclude UI, icons & small assets -- poor AVIF quality, minimal savings".into(),
            ])).style(Style::default().fg(Color::Yellow)), layout[1]);
        }

        // Directory list (Block 0)
        let visible_h = layout[2].height.saturating_sub(2) as usize;
        self.dir_visible_h = visible_h;
        let vis_pos = self.vis_pos().unwrap_or(0);
        if vis_pos < self.scroll_offset {
            self.scroll_offset = vis_pos;
        } else if visible_h > 0 && vis_pos >= self.scroll_offset + visible_h {
            self.scroll_offset = vis_pos - visible_h + 1;
        }
        let scroll = self.scroll_offset;

        {
            let mut cr = self.click.borrow_mut();
            cr.dir_list_area = Some(layout[2]);
            cr.dir_list_scroll = scroll;
        }

        let inner_w = layout[2].width.saturating_sub(2) as usize;
        let items: Vec<ListItem> = self.visible.iter().enumerate()
            .skip(scroll).take(visible_h)
            .map(|(_vi, &di)| {
                let d = &self.dirs[di];
                let is_sel = di == self.selected;
                let dim = d.excluded;
                let sel = if is_sel { "> " } else { "  " };
                let indent = "  ".repeat(d.depth);
                let expand = if d.has_children {
                    if d.expanded { "- " } else { "+ " }
                } else { "  " };
                let mb = d.subtree_bytes as f64 / 1_048_576.0;
                let fixed = 2 + d.depth * 2 + 2 + 5 + 9 + 1 + 3;
                let name_w = inner_w.saturating_sub(fixed).max(8);
                let name_style = if is_sel { Style::default().bold() }
                    else if dim { Style::default().fg(Color::DarkGray) }
                    else { Style::default() };
                let expand_style = if dim { Style::default().fg(Color::DarkGray) }
                    else { Style::default().fg(Color::Yellow) };
                let stat_style = Style::default().fg(if dim { Color::DarkGray } else { Color::Gray });
                let (cb, cb_style) = if d.excluded {
                    ("[ ]", Style::default().fg(Color::Red))
                } else {
                    ("[*]", Style::default().fg(Color::Green))
                };
                ListItem::new(Line::from(vec![
                    Span::styled(sel, if is_sel { Style::default().fg(Color::Cyan) } else { Style::default() }),
                    Span::raw(indent),
                    Span::styled(expand, expand_style),
                    Span::styled(format!("{:<w$}", d.display_name, w = name_w), name_style),
                    Span::styled(format!("{:>5}", d.subtree_count), stat_style),
                    Span::styled(format!("{:>6.0} MB ", mb), stat_style),
                    Span::styled(cb, cb_style),
                ]))
            })
            .collect();
        let dir_border = if self.focus == 0 { Color::Cyan } else { Color::DarkGray };
        let list = List::new(items)
            .block(Block::bordered()
                .title(" Directories ")
                .border_style(Style::default().fg(dir_border)));
        frame.render_widget(list, layout[2]);

        // Quality presets (Block 1)
        let quality_border = if self.focus == 1 { Color::Cyan } else { Color::DarkGray };
        let preset_inner = layout[3].inner(Margin::new(1, 1));
        let preset_cols = Layout::horizontal([
            Constraint::Length(10), // label
            Constraint::Length(8),  // [High]
            Constraint::Length(1),
            Constraint::Length(10), // [Medium]
            Constraint::Length(1),
            Constraint::Length(7),  // [Low]
            Constraint::Length(2),
            Constraint::Min(0),     // description
        ]).split(preset_inner);

        frame.render_widget(Paragraph::new(" Quality:"), preset_cols[0]);
        frame.render_widget(preset_btn(QualityPreset::High, self.preset), preset_cols[1]);
        frame.render_widget(preset_btn(QualityPreset::Medium, self.preset), preset_cols[3]);
        frame.render_widget(preset_btn(QualityPreset::Low, self.preset), preset_cols[5]);
        frame.render_widget(
            Paragraph::new(self.preset.desc().to_string()).style(Style::default().fg(Color::DarkGray)),
            preset_cols[7],
        );

        {
            let mut cr = self.click.borrow_mut();
            cr.preset_high = Some(preset_cols[1]);
            cr.preset_medium = Some(preset_cols[3]);
            cr.preset_low = Some(preset_cols[5]);
        }

        let block = Block::bordered()
            .title(" Quality ")
            .border_style(Style::default().fg(quality_border));
        frame.render_widget(block, layout[3]);

        // Performance block (Block 2)
        let perf_border = if self.focus == 2 { Color::Cyan } else { Color::DarkGray };
        let perf_inner = layout[4].inner(Margin::new(1, 1));
        let perf_cols = Layout::horizontal([
            Constraint::Length(14), // label
            Constraint::Length(7),  // [Low]
            Constraint::Length(1),
            Constraint::Length(10), // [Medium]
            Constraint::Length(1),
            Constraint::Length(8),  // [High]
            Constraint::Length(2),
            Constraint::Min(0),     // description
        ]).split(perf_inner);

        frame.render_widget(Paragraph::new(" Performance:"), perf_cols[0]);
        frame.render_widget(perf_btn("Low", self.worker_tier_label() == "Low"), perf_cols[1]);
        frame.render_widget(perf_btn("Medium", self.worker_tier_label() == "Medium"), perf_cols[3]);
        frame.render_widget(perf_btn("High", self.worker_tier_label() == "High"), perf_cols[5]);
        let perf_desc = format!("{} threads", self.workers);
        frame.render_widget(
            Paragraph::new(perf_desc).style(Style::default().fg(Color::DarkGray)),
            perf_cols[7],
        );

        {
            let mut cr = self.click.borrow_mut();
            cr.perf_low = Some(perf_cols[1]);
            cr.perf_medium = Some(perf_cols[3]);
            cr.perf_high = Some(perf_cols[5]);
        }

        let perf_block = Block::bordered()
            .title(" Performance ")
            .border_style(Style::default().fg(perf_border));
        frame.render_widget(perf_block, layout[4]);

        // Actions block (Block 3): stats line + buttons
        let action_border = if self.focus == 3 { Color::Cyan } else { Color::DarkGray };
        let action_block = Block::bordered()
            .title(" Actions ")
            .border_style(Style::default().fg(action_border));
        let action_inner = action_block.inner(layout[5]);
        frame.render_widget(action_block, layout[5]);

        let action_rows = Layout::vertical([
            Constraint::Length(1), // stats
            Constraint::Length(1), // buttons
        ]).split(action_inner);

        // Stats line
        let (enc_count, enc_bytes) = self.encode_count();
        let skip_count = self.dirs.iter().filter(|d| d.excluded && d.own_count > 0).count();
        frame.render_widget(Paragraph::new(Line::from(vec![
            format!(" {} images", enc_count).green(),
            format!(" ({:.0} MB)", enc_bytes as f64 / 1_048_576.0).dark_gray(),
            "  ".into(),
            format!("{} excluded", skip_count).red(),
        ])), action_rows[0]);

        // Buttons line (right-aligned)
        let btn_cols = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(9),  // [ Start ]
            Constraint::Length(2),
            Constraint::Length(8),  // [ Quit ]
            Constraint::Length(1),
        ]).split(action_rows[1]);

        let start_focused = self.focus == 3 && self.action_idx == 0;
        let quit_focused = self.focus == 3 && self.action_idx == 1;
        frame.render_widget(btn(" Start ", Color::Green, start_focused), btn_cols[1]);
        frame.render_widget(btn(" Quit ", Color::Gray, quit_focused), btn_cols[3]);
        let mut cr = self.click.borrow_mut();
        cr.start_btn = Some(btn_cols[1]);
        cr.quit_btn = Some(btn_cols[3]);

        // Controls hint
        frame.render_widget(Paragraph::new(Line::from(vec![
            " Tab".blue().bold(), " Focus  ".dark_gray(),
            "↑↓←→".blue().bold(), " Navigate  ".dark_gray(),
            "Space".blue().bold(), " Toggle  ".dark_gray(),
            "Enter".blue().bold(), " Activate  ".dark_gray(),
            "q".blue().bold(), " Quit".dark_gray(),
        ])), layout[6]);
    }
    fn draw_building(&self, frame: &mut Frame) {
        let area = frame.area();
        let p = &self.progress;
        let elapsed = self.start_time.elapsed().as_secs_f64();

        let layout = Layout::vertical([
            Constraint::Length(3),  // header
            Constraint::Length(1),  // phase info
            Constraint::Length(3),  // progress bar
            Constraint::Length(7),  // stats
            Constraint::Length(3),  // current file
            Constraint::Min(0),    // warnings
            Constraint::Length(2), // controls
        ]).split(area);

        // Header
        let (status, sc) = if self.cancelling {
            ("Cancelling...", Color::Red)
        } else {
            ("Building", Color::Yellow)
        };
        let header = Paragraph::new(Line::from(vec![
            " renpak ".bold(), "| ".dark_gray(), Span::styled(status, Style::default().fg(sc)),
        ])).block(Block::bordered().border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(header, layout[0]);

        // Phase info
        if !p.phase.is_empty() {
            frame.render_widget(
                Paragraph::new(format!(" {}", p.phase)).style(Style::default().fg(Color::DarkGray)),
                layout[1],
            );
        }

        // Progress bar
        let ratio = if p.total > 0 { p.done as f64 / p.total as f64 } else { 0.0 };
        let gauge = LineGauge::default()
            .filled_style(Style::default().fg(Color::Cyan))
            .unfilled_style(Style::default().fg(Color::DarkGray))
            .label(format!("  {}/{}  {:.0}%", p.done, p.total, ratio * 100.0))
            .ratio(ratio)
            .block(Block::bordered().border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(gauge, layout[2]);

        // Stats
        let eta = if p.done > 0 { (p.total - p.done) as f64 / (p.done as f64 / elapsed) } else { 0.0 };
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
            Line::from(format!("  Original:   {:.0} MB", orig_mb)),
            Line::from(vec![
                format!("  Compressed: {:.0} MB", comp_mb).green(),
                format!(" ({:.0}%)", pct).dark_gray(),
            ]),
            Line::from(format!("  Workers: {}", self.workers).dark_gray()),
        ]).block(Block::bordered().border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(stats, layout[3]);

        // Current file
        let current = Paragraph::new(Line::from(vec![
            "  ".into(), Span::styled(&p.current_file, Style::default().fg(Color::DarkGray)),
        ])).block(Block::bordered().title(" Current ").border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(current, layout[4]);

        // Warnings
        if !p.warnings.is_empty() {
            let warns: Vec<ListItem> = p.warnings.iter().rev().take(5)
                .map(|w| ListItem::new(Span::styled(w.as_str(), Style::default().fg(Color::Yellow))))
                .collect();
            let warn_list = List::new(warns)
                .block(Block::bordered().title(" Warnings ").border_style(Style::default().fg(Color::Yellow)));
            frame.render_widget(warn_list, layout[5]);
        }

        // Controls hint
        let hint = if self.cancelling {
            Paragraph::new(" Waiting for workers to finish...".dark_gray())
        } else {
            Paragraph::new(Line::from(vec![
                " q".blue().bold(), "/".dark_gray(), "Esc".blue().bold(), " Cancel build".dark_gray(),
            ]))
        };
        frame.render_widget(hint, layout[6]);
    }
    fn draw_done(&self, frame: &mut Frame) {
        let result = match &self.phase {
            Phase::Done(r) => r,
            _ => return,
        };
        let area = frame.area();

        let layout = Layout::vertical([
            Constraint::Length(3),  // header
            Constraint::Min(8),    // stats
            Constraint::Length(1), // status message
            Constraint::Length(4), // actions block
            Constraint::Length(1), // controls hint
        ]).split(area);

        match result {
            Ok(stats) if stats.cancelled => {
                let header = Paragraph::new(Line::from(vec![
                    " renpak ".bold(), "| ".dark_gray(), "Cancelled".yellow(),
                ])).block(Block::bordered().border_style(Style::default().fg(Color::Yellow)));
                frame.render_widget(header, layout[0]);

                let n_images = stats.total_entries - stats.passthrough;
                let body = Paragraph::new(vec![
                    Line::from(""),
                    Line::from(format!("  Encoded {}/{} images before cancel",
                        stats.encoded, n_images)),
                    Line::from(format!("  {} cached -- resume will skip these", stats.encoded).dark_gray()),
                    Line::from(""),
                    Line::from("  Press Enter to resume.".yellow()),
                ]).block(Block::bordered().border_style(Style::default().fg(Color::DarkGray)));
                frame.render_widget(body, layout[1]);

                // Actions block
                let action_border = Color::Cyan; // always focused in Done
                let action_block = Block::bordered()
                    .title(" Actions ")
                    .border_style(Style::default().fg(action_border));
                let action_inner = action_block.inner(layout[3]);
                frame.render_widget(action_block, layout[3]);

                let btn_rows = Layout::vertical([
                    Constraint::Length(1), // spacer/info
                    Constraint::Length(1), // buttons
                ]).split(action_inner);

                let btn_cols = Layout::horizontal([
                    Constraint::Min(0),
                    Constraint::Length(10), // [ Resume ]
                    Constraint::Length(2),
                    Constraint::Length(8),  // [ Quit ]
                    Constraint::Length(1),
                ]).split(btn_rows[1]);

                frame.render_widget(btn(" Resume ", Color::Yellow, self.action_idx == 0), btn_cols[1]);
                frame.render_widget(btn(" Quit ", Color::Gray, self.action_idx == 1), btn_cols[3]);
                let mut cr = self.click.borrow_mut();
                cr.start_btn = Some(btn_cols[1]);
                cr.quit_btn = Some(btn_cols[3]);
            }
            Ok(stats) => {
                let header_text = if self.installed { "Installed" } else { "Done" };
                let header = Paragraph::new(Line::from(vec![
                    " renpak ".bold(), "| ".dark_gray(),
                    Span::styled(header_text, Style::default().fg(Color::Green)),
                ])).block(Block::bordered().border_style(Style::default().fg(Color::Green)));
                frame.render_widget(header, layout[0]);

                let rpa_mb = self.rpa_size as f64 / 1_048_576.0;

                let body = if self.already_compressed {
                    // Reopened with already-compressed RPA
                    let backup_path = self.rpa_path.parent().unwrap()
                        .join(".renpak_backup").join(self.rpa_path.file_name().unwrap());
                    let backup_mb = std::fs::metadata(&backup_path)
                        .map(|m| m.len() as f64 / 1_048_576.0).unwrap_or(0.0);
                    let mut lines = vec![
                        Line::from(""),
                        Line::from(format!("  RPA is already compressed ({:.0} MB)", rpa_mb)),
                        Line::from(format!("  {} AVIF images in archive", stats.encoded)),
                    ];
                    if backup_mb > 0.0 {
                        lines.push(Line::from(""));
                        lines.push(Line::from(vec![
                            format!("  Original backup: {:.0} MB", backup_mb).dark_gray(),
                            format!(" ({:.0}%)", rpa_mb / backup_mb * 100.0).dark_gray(),
                        ]));
                    }
                    Paragraph::new(lines)
                        .block(Block::bordered().border_style(Style::default().fg(Color::DarkGray)))
                } else {
                    // Normal build completed
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
                    let t = &stats.timing;
                    Paragraph::new(vec![
                        Line::from(""),
                        Line::from(vec![
                            format!("  RPA:    {:.0} MB -> {:.0} MB", rpa_mb, out_mb).into(),
                            format!(" ({:.0}%)", out_mb / rpa_mb * 100.0).dark_gray(),
                        ]),
                        Line::from(vec![
                            format!("  Images: {:.0} MB -> {:.0} MB", orig_mb, comp_mb).green(),
                            format!(" ({:.0}%)", if orig_mb > 0.0 { comp_mb / orig_mb * 100.0 } else { 0.0 }).dark_gray(),
                        ]),
                        Line::from(""),
                        Line::from(format!("  Encoded: {}  Passthrough: {}  Errors: {}{}",
                            stats.encoded, stats.passthrough, stats.encode_errors,
                            if stats.cache_hits > 0 { format!("  Cached: {}", stats.cache_hits) } else { String::new() })),
                        Line::from(""),
                        Line::from(vec![
                            "  Timing: ".dark_gray(),
                            format!("index {:.1}s", t.index_s).into(),
                            "  pass ".dark_gray(), format!("{:.1}s", t.passthrough_s).into(),
                            "  cache ".dark_gray(), format!("{:.1}s", t.cache_s).into(),
                            "  encode ".dark_gray(), format!("{:.1}s", t.encode_s).into(),
                            "  finalize ".dark_gray(), format!("{:.1}s", t.finalize_s).into(),
                        ]),
                        Line::from(vec![
                            format!("  Total: {}", fmt_duration(t.total_s)).into(),
                        ]),
                    ]).block(Block::bordered().border_style(Style::default().fg(Color::DarkGray)))
                };
                frame.render_widget(body, layout[1]);

                if let Some(msg) = &self.status_msg {
                    let style = if msg.contains("ailed") { Style::default().fg(Color::Red) }
                        else { Style::default().fg(Color::Yellow) };
                    frame.render_widget(Paragraph::new(Span::styled(format!("  {msg}"), style)), layout[2]);
                }

                // Actions block
                let action_border = Color::Cyan; // always focused in Done
                let action_block = Block::bordered()
                    .title(" Actions ")
                    .border_style(Style::default().fg(action_border));
                let action_inner = action_block.inner(layout[3]);
                frame.render_widget(action_block, layout[3]);

                let btn_rows = Layout::vertical([
                    Constraint::Length(1),
                    Constraint::Length(1),
                ]).split(action_inner);

                let mut cr = self.click.borrow_mut();
                if self.installed {
                    let cols = Layout::horizontal([
                        Constraint::Min(0),
                        Constraint::Length(10), // [ Launch ]
                        Constraint::Length(2),
                        Constraint::Length(10), // [ Revert ]
                        Constraint::Length(2),
                        Constraint::Length(10), // [ Delete ]
                        Constraint::Length(2),
                        Constraint::Length(8),  // [ Quit ]
                        Constraint::Length(1),
                    ]).split(btn_rows[1]);
                    frame.render_widget(btn(" Launch ", Color::Cyan, self.action_idx == 0), cols[1]);
                    frame.render_widget(btn(" Revert ", Color::Yellow, self.action_idx == 1), cols[3]);
                    frame.render_widget(btn(" Delete ", Color::Red, self.action_idx == 2), cols[5]);
                    frame.render_widget(btn(" Quit ", Color::Gray, self.action_idx == 3), cols[7]);
                    cr.launch_btn = Some(cols[1]);
                    cr.revert_btn = Some(cols[3]);
                    cr.delete_btn = Some(cols[5]);
                    cr.quit_btn = Some(cols[7]);
                } else {
                    let cols = Layout::horizontal([
                        Constraint::Min(0),
                        Constraint::Length(11), // [ Install ]
                        Constraint::Length(2),
                        Constraint::Length(8),  // [ Quit ]
                        Constraint::Length(1),
                    ]).split(btn_rows[1]);
                    frame.render_widget(btn(" Install ", Color::Green, self.action_idx == 0), cols[1]);
                    frame.render_widget(btn(" Quit ", Color::Gray, self.action_idx == 1), cols[3]);
                    cr.install_btn = Some(cols[1]);
                    cr.quit_btn = Some(cols[3]);
                }
            }
            Err(msg) => {
                let header = Paragraph::new(Line::from(vec![
                    " renpak ".bold(), "| ".dark_gray(), "Error".red(),
                ])).block(Block::bordered().border_style(Style::default().fg(Color::Red)));
                frame.render_widget(header, layout[0]);

                let body = Paragraph::new(vec![
                    Line::from(""), Line::from(format!("  {msg}").red()),
                ]).block(Block::bordered().border_style(Style::default().fg(Color::DarkGray)));
                frame.render_widget(body, layout[1]);
            }
        }

        // Controls hint
        frame.render_widget(Paragraph::new(Line::from(vec![
            " ←→".blue().bold(), " Select  ".dark_gray(),
            "Enter".blue().bold(), " Activate  ".dark_gray(),
            "q".blue().bold(), " Quit".dark_gray(),
        ])), layout[4]);
    }
} // end impl App

// --- Helpers ---

fn fmt_duration(secs: f64) -> String {
    let s = secs as u64;
    if s >= 60 { format!("{}m {:02}s", s / 60, s % 60) } else { format!("{s}s") }
}

fn hit(region: Option<Rect>, col: u16, row: u16) -> bool {
    match region {
        Some(r) => col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height,
        None => false,
    }
}

fn preset_btn(p: QualityPreset, active: QualityPreset) -> Paragraph<'static> {
    let label = format!(" [{}] ", p.label());
    if p == active {
        Paragraph::new(Span::styled(label, Style::default().fg(Color::Black).bg(Color::Cyan).bold()))
    } else {
        Paragraph::new(Span::styled(label, Style::default().fg(Color::DarkGray)))
    }
}

fn perf_btn(label: &str, active: bool) -> Paragraph<'_> {
    let text = format!(" [{}] ", label);
    if active {
        Paragraph::new(Span::styled(text, Style::default().fg(Color::Black).bg(Color::Cyan).bold()))
    } else {
        Paragraph::new(Span::styled(text, Style::default().fg(Color::DarkGray)))
    }
}

fn btn(label: &str, color: Color, focused: bool) -> Paragraph<'_> {
    if focused {
        Paragraph::new(Span::styled(label, Style::default().fg(Color::Black).bg(Color::Cyan).bold()))
    } else {
        Paragraph::new(Span::styled(label, Style::default().fg(Color::Black).bg(color).bold()))
    }
}

impl ClickRegions {
    fn clear(&mut self) { *self = Self::default(); }
}

// --- Entry point ---

pub fn run(game_dir: &Path) -> Result<(), String> {
    let mut app = App::new(game_dir)?;

    crossterm::terminal::enable_raw_mode().map_err(|e| format!("raw mode: {e}"))?;
    execute!(io::stdout(), crossterm::terminal::EnterAlternateScreen, EnableMouseCapture)
        .map_err(|e| format!("terminal init: {e}"))?;

    let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
    let mut terminal = ratatui::Terminal::new(backend).map_err(|e| format!("terminal: {e}"))?;

    let result = run_loop(&mut terminal, &mut app);

    crossterm::terminal::disable_raw_mode().ok();
    execute!(io::stdout(), DisableMouseCapture, crossterm::terminal::LeaveAlternateScreen).ok();

    result.map_err(|e| format!("TUI error: {e}"))
}

fn run_loop(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> io::Result<()> {
    loop {
        terminal.draw(|frame| app.draw(frame))?;

        if app.wants_quit { return Ok(()); }

        if matches!(app.phase, Phase::Building) {
            app.poll_build();
        }

        let timeout = if matches!(app.phase, Phase::Building) {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(200)
        };

        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press { continue; }
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            if matches!(app.phase, Phase::Building) {
                                if !app.cancelling {
                                    app.cancel_flag.store(true, Ordering::Relaxed);
                                    app.cancelling = true;
                                }
                            } else {
                                return Ok(());
                            }
                        }
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            // Check if Quit is the selected action
                            let quit_activated = match &app.phase {
                                Phase::Analyze => app.focus == 3 && app.action_idx == 1,
                                Phase::Done(r) => {
                                    let cancelled = matches!(r, Ok(s) if s.cancelled);
                                    let quit_idx = if cancelled { 1 }
                                        else if app.installed { 3 }
                                        else { 1 };
                                    app.action_idx == quit_idx
                                }
                                _ => false,
                            };
                            if quit_activated {
                                return Ok(());
                            }
                            app.handle_key(key.code, key.modifiers);
                        }
                        code => app.handle_key(code, key.modifiers),
                    }
                }
                Event::Mouse(mouse) => app.handle_mouse(mouse),
                _ => {}
            }
        }
    }
}
