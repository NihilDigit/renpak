//! Build pipeline: parallel AVIF encoding with Rayon.
//!
//! Encode phase streams results directly into the output RPA via Mutex,
//! so memory usage stays bounded (~1 AVIF buffer per worker thread).

use std::collections::{hash_map::DefaultHasher, BTreeMap};
use std::ffi::CString;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::{self, Cursor};
use std::os::raw::c_char;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

#[cfg(unix)]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;

use rayon::prelude::*;
use serde::Serialize;

use crate::manifest::{Manifest, ManifestAsset, MANIFEST_PATH};
use crate::rpa::{RpaEntry, RpaReader, RpaWriter};
use crate::vp9::{
    encode_vp9_bundle, encode_vp9_video, BundleFrame, Vp9BundleOptions, Vp9VideoOptions,
};

// --- Progress callback (C ABI, kept for FFI) ---

#[repr(C)]
pub struct ProgressEvent {
    pub kind: u32, // 0=phase_start, 1=task_done, 2=phase_end, 3=warning
    pub done: u32,
    pub total: u32,
    pub message: *const c_char,
    pub original_bytes: u64,
    pub compressed_bytes: u64,
}

pub type ProgressCb = Option<unsafe extern "C" fn(*const ProgressEvent)>;

fn report(cb: ProgressCb, kind: u32, done: u32, total: u32, msg: &str, orig: u64, comp: u64) {
    if let Some(f) = cb {
        let c = CString::new(msg).unwrap_or_default();
        let ev = ProgressEvent {
            kind,
            done,
            total,
            message: c.as_ptr(),
            original_bytes: orig,
            compressed_bytes: comp,
        };
        unsafe {
            f(&ev);
        }
    }
}

// --- Rust-native progress trait ---

/// Progress reporting for pure-Rust callers (no C ABI overhead).
pub trait ProgressReport: Send + Sync {
    fn phase_start(&self, total: u32, msg: &str);
    fn task_done(&self, done: u32, total: u32, msg: &str, orig: u64, comp: u64);
    fn phase_end(&self, total: u32, msg: &str, orig: u64, comp: u64);
    fn warning(&self, msg: &str);
}

/// No-op progress reporter.
pub struct NoProgress;
impl ProgressReport for NoProgress {
    fn phase_start(&self, _: u32, _: &str) {}
    fn task_done(&self, _: u32, _: u32, _: &str, _: u64, _: u64) {}
    fn phase_end(&self, _: u32, _: &str, _: u64, _: u64) {}
    fn warning(&self, _: &str) {}
}

// --- Classification ---

pub const IMAGE_EXTS: &[&str] = &[".jpg", ".jpeg", ".png", ".webp", ".bmp"];
pub const VIDEO_EXTS: &[&str] = &[".webm", ".mp4", ".mkv", ".avi", ".mov"];
pub const DEFAULT_SKIP_PREFIXES: &[&str] = &["gui/"];
const MAX_PENDING_VP9_GROUPS: usize = 16;

fn is_image_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    IMAGE_EXTS.iter().any(|e| lower.ends_with(e))
}

fn is_skipped_prefix(name: &str, skip_prefixes: &[String]) -> bool {
    let lower = name.to_ascii_lowercase();
    skip_prefixes.iter().any(|p| lower.starts_with(p.as_str()))
}

fn is_video_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    VIDEO_EXTS.iter().any(|e| lower.ends_with(e))
}

pub fn should_encode(name: &str, skip_prefixes: &[String]) -> bool {
    is_image_name(name) && !is_skipped_prefix(name, skip_prefixes)
}

// --- AVIF name helper ---

fn get_avif_name(name: &str) -> String {
    if let Some(pos) = name.rfind('.') {
        format!("{}.avif", &name[..pos])
    } else {
        format!("{name}.avif")
    }
}

// --- AVIF cache (persists across cancel/resume) ---

fn cache_key(name: &str, quality: i32, speed: i32) -> String {
    let mut h = DefaultHasher::new();
    name.hash(&mut h);
    quality.hash(&mut h);
    speed.hash(&mut h);
    format!("{:016x}.avif", h.finish())
}

fn read_cache(cache_dir: &Path, name: &str, quality: i32, speed: i32) -> Option<Vec<u8>> {
    fs::read(cache_dir.join(cache_key(name, quality, speed))).ok()
}

fn write_cache(cache_dir: &Path, name: &str, quality: i32, speed: i32, data: &[u8]) {
    let _ = fs::write(cache_dir.join(cache_key(name, quality, speed)), data);
}

// --- Image decoding ---

fn decode_to_rgba(data: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let img = image::load_from_memory(data).map_err(|e| format!("decode: {e}"))?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok((rgba.into_raw(), w, h))
}

// --- pread helper ---

/// Cross-platform pread: read exact bytes at offset without seeking.
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        file.read_exact_at(buf, offset)
    }
    #[cfg(windows)]
    {
        let mut pos = 0;
        while pos < buf.len() {
            let n = file.seek_read(&mut buf[pos..], offset + pos as u64)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unexpected eof",
                ));
            }
            pos += n;
        }
        Ok(())
    }
}

fn pread_entry(file: &File, entry: &RpaEntry) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; entry.length as usize];
    read_exact_at(file, &mut buf, entry.offset)
        .map_err(|e| format!("pread {}: {e}", entry.name))?;
    if !entry.prefix.is_empty() {
        let mut full = Vec::with_capacity(entry.prefix.len() + buf.len());
        full.extend_from_slice(&entry.prefix);
        full.extend_from_slice(&buf);
        Ok(full)
    } else {
        Ok(buf)
    }
}

// --- Build stats ---

pub struct BuildStats {
    pub total_entries: u32,
    pub encoded: u32,
    pub passthrough: u32,
    pub original_bytes: u64,
    pub compressed_bytes: u64,
    pub encode_errors: u32,
    pub cache_hits: u32,
    pub cancelled: bool,
    pub timing: BuildTiming,
}

#[derive(Default)]
pub struct BuildTiming {
    pub index_s: f64,
    pub passthrough_s: f64,
    pub cache_s: f64,
    pub encode_s: f64,
    pub finalize_s: f64,
    pub total_s: f64,
}

#[derive(Debug, Clone)]
pub struct Vp9BuildOptions {
    pub limit: usize,
    pub video_limit: usize,
    pub bundle_size: usize,
    pub min_width: u32,
    pub min_height: u32,
    pub skip_transparent: bool,
    pub min_savings_percent: u32,
    pub strip_optimized: bool,
    pub optimize_videos: bool,
}

impl Default for Vp9BuildOptions {
    fn default() -> Self {
        Self {
            limit: usize::MAX,
            video_limit: usize::MAX,
            bundle_size: 16,
            min_width: 640,
            min_height: 360,
            skip_transparent: true,
            min_savings_percent: 8,
            strip_optimized: false,
            optimize_videos: true,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Vp9PlanMode {
    Fast,
    Exact,
}

#[derive(Debug, Clone, Serialize)]
pub struct Vp9DimensionStats {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub original_bytes: u64,
    pub planned_bundles: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Vp9PlanStats {
    pub mode: Vp9PlanMode,
    pub total_entries: u64,
    pub image_entries: u64,
    pub excluded_image_entries: u64,
    pub inspected_image_entries: u64,
    pub selected_frames: u64,
    pub selected_original_bytes: u64,
    pub planned_bundles_before_size_guard: u64,
    pub planned_bundles_grouped_by_dimension: u64,
    pub skipped_small: u64,
    pub skipped_transparent: u64,
    pub transparency_unchecked: u64,
    pub decode_errors: u64,
    pub read_errors: u64,
    pub limit_reached: bool,
    pub dimensions: Vec<Vp9DimensionStats>,
}

#[derive(Default)]
struct Vp9DimensionAccum {
    frames: u64,
    original_bytes: u64,
    planned_bundles: u64,
}

#[derive(Debug, Clone)]
struct Vp9CandidateEntry {
    entry: RpaEntry,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone)]
struct Vp9PlanCandidate {
    entry: RpaEntry,
    width: u32,
    height: u32,
    original_bytes: u64,
}

pub fn plan_vp9(
    input_path: &Path,
    options: &Vp9BuildOptions,
    exclude: &[String],
    progress: &dyn ProgressReport,
    cancel: &AtomicBool,
) -> Result<Vp9PlanStats, String> {
    plan_vp9_with_mode(
        input_path,
        options,
        exclude,
        Vp9PlanMode::Fast,
        progress,
        cancel,
    )
}

pub fn plan_vp9_with_mode(
    input_path: &Path,
    options: &Vp9BuildOptions,
    exclude: &[String],
    mode: Vp9PlanMode,
    progress: &dyn ProgressReport,
    cancel: &AtomicBool,
) -> Result<Vp9PlanStats, String> {
    let mut skip_prefixes: Vec<String> = DEFAULT_SKIP_PREFIXES
        .iter()
        .map(|s| s.to_string())
        .collect();
    skip_prefixes.extend(exclude.iter().cloned());

    let mut reader = RpaReader::open(input_path).map_err(|e| format!("open RPA: {e}"))?;
    let entries_map = reader
        .read_index()
        .map_err(|e| format!("read index: {e}"))?;
    let mut entries: Vec<RpaEntry> = entries_map.into_values().collect();
    entries.sort_by_key(|e| e.offset);
    let src_file = reader.file();
    let frame_limit = options.limit.max(1) as u64;
    let bundle_size = options.bundle_size.clamp(1, 32);

    let mut stats = Vp9PlanStats {
        mode: mode.clone(),
        total_entries: entries.len() as u64,
        image_entries: 0,
        excluded_image_entries: 0,
        inspected_image_entries: 0,
        selected_frames: 0,
        selected_original_bytes: 0,
        planned_bundles_before_size_guard: 0,
        planned_bundles_grouped_by_dimension: 0,
        skipped_small: 0,
        skipped_transparent: 0,
        transparency_unchecked: 0,
        decode_errors: 0,
        read_errors: 0,
        limit_reached: false,
        dimensions: Vec::new(),
    };
    let mut candidates: Vec<Vp9PlanCandidate> = Vec::new();

    progress.phase_start(
        entries.len() as u32,
        &format!(
            "Planning VP9 candidates ({:?}, bundle-size {}, min {}x{})",
            mode, bundle_size, options.min_width, options.min_height
        ),
    );

    for (done, entry) in entries.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !is_image_name(&entry.name) {
            continue;
        }
        stats.image_entries += 1;
        if is_skipped_prefix(&entry.name, &skip_prefixes) {
            stats.excluded_image_entries += 1;
            continue;
        }

        let raw = match pread_entry(src_file, entry) {
            Ok(raw) => raw,
            Err(_) => {
                stats.read_errors += 1;
                continue;
            }
        };
        let original_bytes = raw.len() as u64;
        let (width, height, _) = match inspect_vp9_plan_image(&raw, &Vp9PlanMode::Fast) {
            Ok(inspected) => inspected,
            Err(_) => {
                stats.decode_errors += 1;
                continue;
            }
        };
        stats.inspected_image_entries += 1;

        if width < options.min_width || height < options.min_height {
            stats.skipped_small += 1;
            continue;
        }
        candidates.push(Vp9PlanCandidate {
            entry: entry.clone(),
            width,
            height,
            original_bytes,
        });

        let done = (done + 1) as u32;
        if done.is_multiple_of(1000) {
            progress.task_done(done, entries.len() as u32, &entry.name, 0, 0);
        }
    }

    sort_vp9_plan_candidates(&mut candidates);

    let mut dimensions: BTreeMap<(u32, u32), Vp9DimensionAccum> = BTreeMap::new();
    let mut pending_counts: BTreeMap<(u32, u32), usize> = BTreeMap::new();
    for candidate in &candidates {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if stats.selected_frames >= frame_limit {
            stats.limit_reached = true;
            break;
        }

        let mut width = candidate.width;
        let mut height = candidate.height;
        let mut original_bytes = candidate.original_bytes;
        match mode {
            Vp9PlanMode::Fast => {
                if options.skip_transparent {
                    stats.transparency_unchecked += 1;
                }
            }
            Vp9PlanMode::Exact => {
                let raw = match pread_entry(src_file, &candidate.entry) {
                    Ok(raw) => raw,
                    Err(_) => {
                        stats.read_errors += 1;
                        continue;
                    }
                };
                original_bytes = raw.len() as u64;
                let (rgba, decoded_width, decoded_height) = match decode_to_rgba(&raw) {
                    Ok(decoded) => decoded,
                    Err(_) => {
                        stats.decode_errors += 1;
                        continue;
                    }
                };
                width = decoded_width;
                height = decoded_height;
                if width < options.min_width || height < options.min_height {
                    stats.skipped_small += 1;
                    continue;
                }
                if options.skip_transparent && rgba_has_transparency(&rgba) {
                    stats.skipped_transparent += 1;
                    continue;
                }
            }
        }

        let key = (width, height);
        let dim = dimensions.entry(key).or_default();
        dim.frames += 1;
        dim.original_bytes += original_bytes;
        stats.selected_frames += 1;
        stats.selected_original_bytes += original_bytes;
        plan_vp9_pending_frame(&mut pending_counts, &mut dimensions, key, bundle_size);
    }

    let pending_keys: Vec<(u32, u32)> = pending_counts.keys().copied().collect();
    for key in pending_keys {
        plan_vp9_flush_pending(&mut pending_counts, &mut dimensions, key);
    }

    stats.planned_bundles_before_size_guard =
        dimensions.values().map(|dim| dim.planned_bundles).sum();
    stats.planned_bundles_grouped_by_dimension = dimensions
        .values()
        .map(|dim| dim.frames.div_ceil(bundle_size as u64))
        .sum();
    stats.dimensions = dimensions
        .into_iter()
        .map(|((width, height), dim)| Vp9DimensionStats {
            width,
            height,
            frames: dim.frames,
            original_bytes: dim.original_bytes,
            planned_bundles: dim.planned_bundles,
        })
        .collect();
    stats.dimensions.sort_by(|a, b| {
        b.frames
            .cmp(&a.frames)
            .then_with(|| b.original_bytes.cmp(&a.original_bytes))
            .then_with(|| a.width.cmp(&b.width))
            .then_with(|| a.height.cmp(&b.height))
    });

    progress.phase_end(
        stats.total_entries as u32,
        "VP9 plan complete",
        stats.selected_original_bytes,
        0,
    );
    Ok(stats)
}

fn inspect_vp9_plan_image(
    data: &[u8],
    mode: &Vp9PlanMode,
) -> Result<(u32, u32, Option<bool>), String> {
    match mode {
        Vp9PlanMode::Exact => {
            let (rgba, width, height) = decode_to_rgba(data)?;
            Ok((width, height, Some(rgba_has_transparency(&rgba))))
        }
        Vp9PlanMode::Fast => {
            let reader = image::ImageReader::new(Cursor::new(data))
                .with_guessed_format()
                .map_err(|e| format!("image header: {e}"))?;
            let (width, height) = reader
                .into_dimensions()
                .map_err(|e| format!("image dimensions: {e}"))?;
            Ok((width, height, None))
        }
    }
}

fn plan_vp9_pending_frame(
    pending_counts: &mut BTreeMap<(u32, u32), usize>,
    dimensions: &mut BTreeMap<(u32, u32), Vp9DimensionAccum>,
    key: (u32, u32),
    bundle_size: usize,
) {
    let should_flush = {
        let count = pending_counts.entry(key).or_default();
        *count += 1;
        *count >= bundle_size
    };
    if should_flush {
        plan_vp9_flush_pending(pending_counts, dimensions, key);
    }

    while pending_counts.len() > MAX_PENDING_VP9_GROUPS {
        if let Some(overflow_key) = pending_counts.keys().next().copied() {
            plan_vp9_flush_pending(pending_counts, dimensions, overflow_key);
        } else {
            break;
        }
    }
}

fn plan_vp9_flush_pending(
    pending_counts: &mut BTreeMap<(u32, u32), usize>,
    dimensions: &mut BTreeMap<(u32, u32), Vp9DimensionAccum>,
    key: (u32, u32),
) {
    if pending_counts.remove(&key).unwrap_or(0) > 0 {
        if let Some(dim) = dimensions.get_mut(&key) {
            dim.planned_bundles += 1;
        }
    }
}

fn sort_vp9_candidate_entries(candidates: &mut [Vp9CandidateEntry]) {
    let mut dimension_counts: BTreeMap<(u32, u32), usize> = BTreeMap::new();
    for candidate in candidates.iter() {
        *dimension_counts
            .entry((candidate.width, candidate.height))
            .or_default() += 1;
    }
    candidates.sort_by(|a, b| {
        compare_vp9_candidate_order(
            (a.width, a.height),
            &a.entry.name,
            a.entry.offset,
            (b.width, b.height),
            &b.entry.name,
            b.entry.offset,
            &dimension_counts,
        )
    });
}

fn sort_vp9_plan_candidates(candidates: &mut [Vp9PlanCandidate]) {
    let mut dimension_counts: BTreeMap<(u32, u32), usize> = BTreeMap::new();
    for candidate in candidates.iter() {
        *dimension_counts
            .entry((candidate.width, candidate.height))
            .or_default() += 1;
    }
    candidates.sort_by(|a, b| {
        compare_vp9_candidate_order(
            (a.width, a.height),
            &a.entry.name,
            a.entry.offset,
            (b.width, b.height),
            &b.entry.name,
            b.entry.offset,
            &dimension_counts,
        )
    });
}

fn compare_vp9_candidate_order(
    a_dim: (u32, u32),
    a_name: &str,
    a_offset: u64,
    b_dim: (u32, u32),
    b_name: &str,
    b_offset: u64,
    dimension_counts: &BTreeMap<(u32, u32), usize>,
) -> std::cmp::Ordering {
    let a_count = dimension_counts.get(&a_dim).copied().unwrap_or(0);
    let b_count = dimension_counts.get(&b_dim).copied().unwrap_or(0);
    b_count
        .cmp(&a_count)
        .then_with(|| a_dim.0.cmp(&b_dim.0))
        .then_with(|| a_dim.1.cmp(&b_dim.1))
        .then_with(|| natural_ascii_cmp(a_name, b_name))
        .then_with(|| a_offset.cmp(&b_offset))
}

fn natural_ascii_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let mut ai = 0usize;
    let mut bi = 0usize;

    while ai < a_bytes.len() && bi < b_bytes.len() {
        let ac = a_bytes[ai];
        let bc = b_bytes[bi];
        if ac.is_ascii_digit() && bc.is_ascii_digit() {
            let (a_start, mut a_end) = (ai, ai);
            let (b_start, mut b_end) = (bi, bi);
            while a_end < a_bytes.len() && a_bytes[a_end].is_ascii_digit() {
                a_end += 1;
            }
            while b_end < b_bytes.len() && b_bytes[b_end].is_ascii_digit() {
                b_end += 1;
            }

            let a_trim = skip_ascii_zeroes(&a_bytes[a_start..a_end]);
            let b_trim = skip_ascii_zeroes(&b_bytes[b_start..b_end]);
            let number_order = a_trim
                .len()
                .cmp(&b_trim.len())
                .then_with(|| a_trim.cmp(b_trim));
            if number_order != std::cmp::Ordering::Equal {
                return number_order;
            }

            let width_order = (a_end - a_start).cmp(&(b_end - b_start));
            if width_order != std::cmp::Ordering::Equal {
                return width_order;
            }
            ai = a_end;
            bi = b_end;
        } else {
            let char_order = ac.cmp(&bc);
            if char_order != std::cmp::Ordering::Equal {
                return char_order;
            }
            ai += 1;
            bi += 1;
        }
    }

    a_bytes.len().cmp(&b_bytes.len())
}

fn skip_ascii_zeroes(bytes: &[u8]) -> &[u8] {
    let mut i = 0usize;
    while i + 1 < bytes.len() && bytes[i] == b'0' {
        i += 1;
    }
    &bytes[i..]
}

fn collect_vp9_build_candidates(
    src_file: &File,
    entries: &[RpaEntry],
    options: &Vp9BuildOptions,
    skip_prefixes: &[String],
    progress: &dyn ProgressReport,
    cancel: &AtomicBool,
) -> Result<Vec<Vp9CandidateEntry>, String> {
    let mut candidates = Vec::new();
    progress.phase_start(entries.len() as u32, "Indexing VP9 image candidates");

    for (done, entry) in entries.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !should_encode(&entry.name, skip_prefixes) {
            continue;
        }

        let raw = match pread_entry(src_file, entry) {
            Ok(raw) => raw,
            Err(msg) => {
                progress.warning(&msg);
                continue;
            }
        };
        let (width, height, _) = match inspect_vp9_plan_image(&raw, &Vp9PlanMode::Fast) {
            Ok(inspected) => inspected,
            Err(msg) => {
                progress.warning(&format!("inspect {}: {msg}", entry.name));
                continue;
            }
        };
        if width < options.min_width || height < options.min_height {
            continue;
        }

        candidates.push(Vp9CandidateEntry {
            entry: entry.clone(),
            width,
            height,
        });

        let done = (done + 1) as u32;
        if done.is_multiple_of(1000) {
            progress.task_done(done, entries.len() as u32, &entry.name, 0, 0);
        }
    }

    sort_vp9_candidate_entries(&mut candidates);

    progress.phase_end(
        candidates.len() as u32,
        &format!("Indexed {} VP9 candidates", candidates.len()),
        0,
        0,
    );
    Ok(candidates)
}

#[allow(clippy::too_many_arguments)]
pub fn build_vp9_sample(
    input_path: &Path,
    output_path: &Path,
    limit: usize,
    bundle_size: usize,
    work_dir: &Path,
    android_assets_dir: Option<&Path>,
    exclude: &[String],
    progress: &dyn ProgressReport,
    cancel: &AtomicBool,
) -> Result<BuildStats, String> {
    let options = Vp9BuildOptions {
        limit,
        video_limit: 0,
        bundle_size,
        min_width: 1,
        min_height: 1,
        skip_transparent: false,
        min_savings_percent: 0,
        strip_optimized: false,
        optimize_videos: false,
    };
    build_vp9(
        input_path,
        output_path,
        &options,
        work_dir,
        android_assets_dir,
        exclude,
        progress,
        cancel,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn build_vp9(
    input_path: &Path,
    output_path: &Path,
    options: &Vp9BuildOptions,
    work_dir: &Path,
    android_assets_dir: Option<&Path>,
    exclude: &[String],
    progress: &dyn ProgressReport,
    cancel: &AtomicBool,
) -> Result<BuildStats, String> {
    let t_total = Instant::now();
    let mut skip_prefixes: Vec<String> = DEFAULT_SKIP_PREFIXES
        .iter()
        .map(|s| s.to_string())
        .collect();
    skip_prefixes.extend(exclude.iter().cloned());

    let t0 = Instant::now();
    let mut reader = RpaReader::open(input_path).map_err(|e| format!("open RPA: {e}"))?;
    let mut index = reader
        .read_index()
        .map_err(|e| format!("read index: {e}"))?;
    let src_key = reader.key();
    let dt_index = t0.elapsed().as_secs_f64();

    let mut entries: Vec<RpaEntry> = index.drain().map(|(_, entry)| entry).collect();
    entries.sort_by_key(|e| e.offset);

    let src_file = reader.file();
    let frame_limit = options.limit;
    let bundle_size = options.bundle_size.clamp(1, 32);
    let encode_options = Vp9BundleOptions::default();
    let candidates = if frame_limit == 0 {
        Vec::new()
    } else {
        collect_vp9_build_candidates(
            src_file,
            &entries,
            options,
            &skip_prefixes,
            progress,
            cancel,
        )?
    };
    let t_encode = Instant::now();
    let progress_total = u32::try_from(frame_limit.min(candidates.len())).unwrap_or(u32::MAX);
    let mut pending_frames: BTreeMap<(u32, u32), Vec<BundleFrame>> = BTreeMap::new();
    let mut encoded_bundles: Vec<(String, Vec<u8>)> = Vec::new();
    let mut manifest = Manifest::new();
    let mut selected_frames = 0usize;
    let mut encoded_frames = 0u32;
    let mut compressed_bytes = 0u64;

    if frame_limit > 0 {
        progress.phase_start(
            progress_total,
            &format!(
                "Selecting and encoding VP9 bundles (bundle-size {}, gop {}, crf {})",
                bundle_size, encode_options.gop, encode_options.crf
            ),
        );
    }
    for candidate in &candidates {
        let entry = &candidate.entry;
        if cancel.load(Ordering::Relaxed) {
            return Ok(cancelled_stats(entries.len() as u32, 0, 0));
        }
        if selected_frames >= frame_limit {
            break;
        }

        let raw = match pread_entry(src_file, entry) {
            Ok(raw) => raw,
            Err(msg) => {
                progress.warning(&msg);
                continue;
            }
        };
        let original_bytes = raw.len() as u64;
        let (rgba, width, height) = match decode_to_rgba(&raw) {
            Ok(decoded) => decoded,
            Err(msg) => {
                progress.warning(&format!("decode {}: {msg}", entry.name));
                continue;
            }
        };
        if width < options.min_width || height < options.min_height {
            continue;
        }
        if width != candidate.width || height != candidate.height {
            progress.warning(&format!(
                "image dimensions changed during decode {}: planned {}x{}, decoded {}x{}",
                entry.name, candidate.width, candidate.height, width, height
            ));
        }
        if options.skip_transparent && rgba_has_transparency(&rgba) {
            progress.warning(&format!("skip transparent image {}", entry.name));
            continue;
        }

        let frame = BundleFrame {
            name: entry.name.clone(),
            original_bytes,
            rgba,
            width,
            height,
        };
        let group_key = (width, height);
        let should_flush_group = {
            let group = pending_frames.entry(group_key).or_default();
            group.push(frame);
            group.len() >= bundle_size
        };
        selected_frames += 1;
        progress.task_done(
            selected_frames as u32,
            progress_total,
            &entry.name,
            0,
            compressed_bytes,
        );

        if should_flush_group {
            encode_vp9_pending_group(
                &mut pending_frames,
                group_key,
                work_dir,
                &encode_options,
                options.min_savings_percent,
                &mut encoded_bundles,
                &mut manifest,
                &mut encoded_frames,
                &mut compressed_bytes,
                progress,
                selected_frames as u32,
                progress_total,
            )?;
        }

        while pending_frames.len() > MAX_PENDING_VP9_GROUPS {
            let overflow_key = pending_frames
                .keys()
                .next()
                .copied()
                .ok_or_else(|| "internal VP9 pending group error".to_string())?;
            encode_vp9_pending_group(
                &mut pending_frames,
                overflow_key,
                work_dir,
                &encode_options,
                options.min_savings_percent,
                &mut encoded_bundles,
                &mut manifest,
                &mut encoded_frames,
                &mut compressed_bytes,
                progress,
                selected_frames as u32,
                progress_total,
            )?;
        }
    }

    let pending_keys: Vec<(u32, u32)> = pending_frames.keys().copied().collect();
    for key in pending_keys {
        if cancel.load(Ordering::Relaxed) {
            return Ok(cancelled_stats(entries.len() as u32, 0, encoded_frames));
        }
        encode_vp9_pending_group(
            &mut pending_frames,
            key,
            work_dir,
            &encode_options,
            options.min_savings_percent,
            &mut encoded_bundles,
            &mut manifest,
            &mut encoded_frames,
            &mut compressed_bytes,
            progress,
            selected_frames as u32,
            progress_total,
        )?;
    }
    let dt_encode = t_encode.elapsed().as_secs_f64();
    progress.phase_end(
        selected_frames as u32,
        &format!(
            "VP9 selection/encoding done ({} bundles, {:.1}s)",
            encoded_bundles.len(),
            dt_encode
        ),
        0,
        compressed_bytes,
    );

    let mut encoded_videos = Vec::new();
    if options.optimize_videos && options.video_limit > 0 {
        encoded_videos = encode_mobile_videos(
            src_file,
            &entries,
            options,
            &skip_prefixes,
            work_dir,
            &mut manifest,
            &mut compressed_bytes,
            progress,
            cancel,
        )?;
    }
    if selected_frames == 0 && encoded_videos.is_empty() {
        return Err("no compatible image or video assets found for VP9 mobile profile".to_string());
    }

    let mut writer =
        RpaWriter::create(output_path, src_key).map_err(|e| format!("create output RPA: {e}"))?;

    let strip_count = if options.strip_optimized {
        manifest.assets.len()
    } else {
        0
    };
    let copy_total = entries.len().saturating_sub(strip_count) as u32;
    progress.phase_start(copy_total, "Copying source entries");
    let t_pass = Instant::now();
    let mut copy_buf = vec![0u8; 1024 * 1024];
    let mut copied_entries = 0u32;
    for entry in &entries {
        if cancel.load(Ordering::Relaxed) {
            return Ok(cancelled_stats(entries.len() as u32, copied_entries, 0));
        }
        if options.strip_optimized && manifest.assets.contains_key(&entry.name) {
            continue;
        }
        writer
            .add_file_from(
                &entry.name,
                src_file,
                entry.offset,
                entry.length,
                &entry.prefix,
                &mut copy_buf,
            )
            .map_err(|e| format!("copy '{}': {e}", entry.name))?;
        copied_entries += 1;
        if copied_entries.is_multiple_of(500) || copied_entries == copy_total {
            progress.task_done(copied_entries, copy_total, &entry.name, 0, 0);
        }
    }
    let dt_pass = t_pass.elapsed().as_secs_f64();
    progress.phase_end(
        copied_entries,
        &format!("Source entries copied ({:.1}s)", dt_pass),
        0,
        0,
    );

    for (bundle_target, bundle_data) in &encoded_bundles {
        writer
            .add_file(bundle_target, bundle_data)
            .map_err(|e| format!("write VP9 bundle: {e}"))?;
    }

    for video in &encoded_videos {
        writer
            .add_file(&video.target, &video.data)
            .map_err(|e| format!("write VP9 video: {e}"))?;
    }

    let manifest_json = build_manifest_json(&manifest);
    writer
        .add_file(MANIFEST_PATH, manifest_json.as_bytes())
        .map_err(|e| format!("write manifest: {e}"))?;

    if let Some(dir) = android_assets_dir {
        write_android_assets(dir, &encoded_bundles, &manifest_json)?;
    }

    let t_finalize = Instant::now();
    writer.finish().map_err(|e| format!("finalize RPA: {e}"))?;
    let dt_finalize = t_finalize.elapsed().as_secs_f64();
    let dt_total = t_total.elapsed().as_secs_f64();

    Ok(BuildStats {
        total_entries: entries.len() as u32,
        encoded: encoded_frames + encoded_videos.len() as u32,
        passthrough: copied_entries,
        original_bytes: 0,
        compressed_bytes,
        encode_errors: 0,
        cache_hits: 0,
        cancelled: false,
        timing: BuildTiming {
            index_s: dt_index,
            passthrough_s: dt_pass,
            cache_s: 0.0,
            encode_s: dt_encode,
            finalize_s: dt_finalize,
            total_s: dt_total,
        },
    })
}

struct EncodedVideoAsset {
    target: String,
    data: Vec<u8>,
}

#[allow(clippy::too_many_arguments)]
fn encode_mobile_videos(
    src_file: &File,
    entries: &[RpaEntry],
    options: &Vp9BuildOptions,
    skip_prefixes: &[String],
    work_dir: &Path,
    manifest: &mut Manifest,
    compressed_bytes: &mut u64,
    progress: &dyn ProgressReport,
    cancel: &AtomicBool,
) -> Result<Vec<EncodedVideoAsset>, String> {
    let video_entries: Vec<&RpaEntry> = entries
        .iter()
        .filter(|entry| {
            is_video_name(&entry.name) && !is_skipped_prefix(&entry.name, skip_prefixes)
        })
        .take(options.video_limit)
        .collect();

    if video_entries.is_empty() {
        return Ok(Vec::new());
    }

    let video_options = Vp9VideoOptions::default();
    let mut encoded = Vec::new();
    let mut original_total = 0u64;
    let mut compressed_total = 0u64;
    let total = u32::try_from(video_entries.len()).unwrap_or(u32::MAX);
    progress.phase_start(
        total,
        &format!(
            "Encoding mobile VP9 videos (720p cap, crf {}, realtime)",
            video_options.crf
        ),
    );

    for (index, entry) in video_entries.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }

        let raw = match pread_entry(src_file, entry) {
            Ok(raw) => raw,
            Err(msg) => {
                progress.warning(&msg);
                continue;
            }
        };
        let original_bytes = raw.len() as u64;
        let video = match encode_vp9_video(&entry.name, &raw, work_dir, &video_options) {
            Ok(video) => video,
            Err(msg) => {
                progress.warning(&msg);
                continue;
            }
        };
        let encoded_bytes = video.data.len() as u64;
        if !vp9_bundle_saves_enough(original_bytes, encoded_bytes, options.min_savings_percent) {
            progress.warning(&format!(
                "skip VP9 video {}: {} -> {} bytes saves less than {}%",
                entry.name, original_bytes, encoded_bytes, options.min_savings_percent
            ));
            continue;
        }

        let target = format!("renpak/videos/v{:03}.webm", encoded.len());
        manifest.insert(
            entry.name.clone(),
            ManifestAsset::video_vp9(target.clone(), None, None, video_options.profile.clone()),
        );
        original_total += original_bytes;
        compressed_total += encoded_bytes;
        *compressed_bytes += encoded_bytes;
        encoded.push(EncodedVideoAsset {
            target: target.clone(),
            data: video.data,
        });

        progress.task_done(
            (index + 1) as u32,
            total,
            &entry.name,
            original_total,
            compressed_total,
        );
    }

    progress.phase_end(
        total,
        &format!("Mobile video encoding done ({} videos)", encoded.len()),
        original_total,
        compressed_total,
    );
    Ok(encoded)
}

fn rgba_has_transparency(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4).any(|px| px[3] != 255)
}

fn vp9_bundle_saves_enough(
    original_bytes: u64,
    bundle_bytes: u64,
    min_savings_percent: u32,
) -> bool {
    if min_savings_percent == 0 {
        return true;
    }
    if original_bytes == 0 {
        return false;
    }

    let required = 100u128.saturating_sub(min_savings_percent as u128);
    (bundle_bytes as u128) * 100 <= (original_bytes as u128) * required
}

#[allow(clippy::too_many_arguments)]
fn encode_vp9_pending_group(
    pending_frames: &mut BTreeMap<(u32, u32), Vec<BundleFrame>>,
    group_key: (u32, u32),
    work_dir: &Path,
    encode_options: &Vp9BundleOptions,
    min_savings_percent: u32,
    encoded_bundles: &mut Vec<(String, Vec<u8>)>,
    manifest: &mut Manifest,
    encoded_frames: &mut u32,
    compressed_bytes: &mut u64,
    progress: &dyn ProgressReport,
    progress_done: u32,
    progress_total: u32,
) -> Result<(), String> {
    let chunk = pending_frames.remove(&group_key).unwrap_or_default();
    if chunk.is_empty() {
        return Ok(());
    }

    let bundle_index = encoded_bundles.len();
    let bundle_target = format!("renpak/bundles/b{bundle_index:03}.webm");
    let bundle = encode_vp9_bundle(&chunk, work_dir, encode_options)?;
    let bundle_bytes = bundle.data.len() as u64;
    let original_bytes: u64 = chunk.iter().map(|frame| frame.original_bytes).sum();

    if !vp9_bundle_saves_enough(original_bytes, bundle_bytes, min_savings_percent) {
        progress.warning(&format!(
            "skip VP9 bundle {}: {} -> {} bytes saves less than {}%",
            bundle_target, original_bytes, bundle_bytes, min_savings_percent
        ));
        return Ok(());
    }

    *compressed_bytes += bundle_bytes;
    for (frame, original) in chunk.iter().enumerate() {
        manifest.insert(
            original.name.clone(),
            ManifestAsset::vp9_bundle_frame(
                bundle_target.clone(),
                frame as u32,
                bundle.width,
                bundle.height,
                encode_options.gop,
                encode_options.profile.clone(),
            ),
        );
    }

    *encoded_frames += bundle.frame_count;
    progress.task_done(
        progress_done,
        progress_total,
        &bundle_target,
        0,
        *compressed_bytes,
    );
    encoded_bundles.push((bundle_target, bundle.data));
    Ok(())
}

fn write_android_assets(
    assets_dir: &Path,
    bundles: &[(String, Vec<u8>)],
    manifest_json: &str,
) -> Result<(), String> {
    fs::create_dir_all(assets_dir).map_err(|e| format!("create Android assets dir: {e}"))?;
    for (bundle_target, bundle_data) in bundles {
        let bundle_path = assets_dir.join(bundle_target);
        let bundle_parent = bundle_path
            .parent()
            .ok_or_else(|| format!("invalid bundle target: {bundle_target}"))?;
        fs::create_dir_all(bundle_parent).map_err(|e| format!("create Android assets dir: {e}"))?;
        fs::write(&bundle_path, bundle_data)
            .map_err(|e| format!("write Android VP9 bundle: {e}"))?;
    }
    fs::write(assets_dir.join(MANIFEST_PATH), manifest_json)
        .map_err(|e| format!("write Android manifest: {e}"))?;
    Ok(())
}

fn cancelled_stats(total_entries: u32, passthrough: u32, encoded: u32) -> BuildStats {
    BuildStats {
        total_entries,
        encoded,
        passthrough,
        original_bytes: 0,
        compressed_bytes: 0,
        encode_errors: 0,
        cache_hits: 0,
        cancelled: true,
        timing: BuildTiming::default(),
    }
}

// PLACEHOLDER_BUILD_MAIN

/// Build a compressed RPA: read source → passthrough copy → parallel AVIF encode + write.
///
/// Encoded AVIF data is written to the output RPA immediately via Mutex,
/// keeping memory usage bounded to ~1 buffer per worker thread.
#[allow(clippy::too_many_arguments)]
pub fn build(
    input_path: &Path,
    output_path: &Path,
    quality: i32,
    speed: i32,
    workers: usize,
    exclude: &[String],
    progress: &dyn ProgressReport,
    cancel: &AtomicBool,
    cache_dir: Option<&Path>,
) -> Result<BuildStats, String> {
    // 1. Build skip prefixes: defaults + user excludes
    let t_total = Instant::now();
    let mut skip_prefixes: Vec<String> = DEFAULT_SKIP_PREFIXES
        .iter()
        .map(|s| s.to_string())
        .collect();
    skip_prefixes.extend(exclude.iter().cloned());

    // 2. Read source index
    let t0 = Instant::now();
    let mut reader = RpaReader::open(input_path).map_err(|e| format!("open RPA: {e}"))?;
    let index = reader
        .read_index()
        .map_err(|e| format!("read index: {e}"))?;
    let src_key = reader.key();
    let dt_index = t0.elapsed().as_secs_f64();

    // 3. Classify entries
    let mut to_encode: Vec<&RpaEntry> = Vec::new();
    let mut to_passthrough: Vec<&RpaEntry> = Vec::new();
    for entry in index.values() {
        if should_encode(&entry.name, &skip_prefixes) {
            to_encode.push(entry);
        } else {
            to_passthrough.push(entry);
        }
    }

    let n_encode = to_encode.len() as u32;
    let n_pass = to_passthrough.len() as u32;

    // Sort passthrough by offset for sequential I/O on source RPA
    to_passthrough.sort_by_key(|e| e.offset);

    // 4. Create output RPA and write passthrough entries first
    let mut writer =
        RpaWriter::create(output_path, src_key).map_err(|e| format!("create output RPA: {e}"))?;

    progress.phase_start(n_pass, &format!("Copying {} passthrough entries", n_pass));
    let src_file = reader.file();
    let mut copy_buf = vec![0u8; 1024 * 1024]; // 1MB reusable buffer
    let t0 = Instant::now();
    for (i, entry) in to_passthrough.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(BuildStats {
                total_entries: n_encode + n_pass,
                encoded: 0,
                passthrough: i as u32,
                original_bytes: 0,
                compressed_bytes: 0,
                encode_errors: 0,
                cache_hits: 0,
                cancelled: true,
                timing: BuildTiming::default(),
            });
        }
        writer
            .add_file_from(
                &entry.name,
                src_file,
                entry.offset,
                entry.length,
                &entry.prefix,
                &mut copy_buf,
            )
            .map_err(|e| format!("copy '{}': {e}", entry.name))?;

        if (i + 1) % 500 == 0 || i + 1 == to_passthrough.len() {
            progress.task_done(
                (i + 1) as u32,
                n_pass,
                &format!("Copied {}/{}", i + 1, n_pass),
                0,
                0,
            );
        }
    }
    let dt_pass = t0.elapsed().as_secs_f64();
    let pass_mb = to_passthrough
        .iter()
        .map(|e| e.length + e.prefix.len() as u64)
        .sum::<u64>() as f64
        / 1_048_576.0;
    progress.phase_end(
        n_pass,
        &format!(
            "Passthrough done ({:.1}s, {:.0} MB, {:.0} MB/s)",
            dt_pass,
            pass_mb,
            pass_mb / dt_pass.max(0.001)
        ),
        0,
        0,
    );

    // 5. Split encode list: cached vs fresh
    let mut cached_entries: Vec<&RpaEntry> = Vec::new();
    let mut fresh_entries: Vec<&RpaEntry> = Vec::new();
    for entry in &to_encode {
        if let Some(cd) = cache_dir {
            if cd.join(cache_key(&entry.name, quality, speed)).exists() {
                cached_entries.push(entry);
                continue;
            }
        }
        fresh_entries.push(entry);
    }
    let n_cached = cached_entries.len() as u32;
    let n_fresh = fresh_entries.len() as u32;

    let mut manifest = Manifest::new();
    let mut orig_total: u64 = 0;
    let mut comp_total: u64 = 0;

    // 5a. Restore cached entries (sequential, fast I/O only)
    let mut dt_cache = 0.0f64;
    if n_cached > 0 {
        progress.phase_start(n_cached, &format!("Restoring {} cached images", n_cached));
        let t0 = Instant::now();
        for (i, entry) in cached_entries.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(BuildStats {
                    total_entries: n_encode + n_pass,
                    encoded: i as u32,
                    passthrough: n_pass,
                    original_bytes: orig_total,
                    compressed_bytes: comp_total,
                    encode_errors: 0,
                    cache_hits: i as u32,
                    cancelled: true,
                    timing: BuildTiming::default(),
                });
            }
            let avif_name = get_avif_name(&entry.name);
            let cached = read_cache(cache_dir.unwrap(), &entry.name, quality, speed)
                .ok_or_else(|| format!("cache miss for {}", entry.name))?;
            let orig_bytes = entry.length + entry.prefix.len() as u64;
            let comp_bytes = cached.len() as u64;
            writer
                .add_file(&avif_name, &cached)
                .map_err(|e| format!("write cached '{}': {e}", avif_name))?;
            manifest.insert(
                entry.name.clone(),
                ManifestAsset::avif(avif_name.clone(), None, None, "legacy-avif".to_string()),
            );
            orig_total += orig_bytes;
            comp_total += comp_bytes;
            if (i + 1) % 100 == 0 || i + 1 == cached_entries.len() {
                progress.task_done((i + 1) as u32, n_cached, &avif_name, orig_total, comp_total);
            }
        }
        dt_cache = t0.elapsed().as_secs_f64();
        let cache_mb = comp_total as f64 / 1_048_576.0;
        progress.phase_end(
            n_cached,
            &format!(
                "Cache restored ({:.1}s, {:.0} MB, {:.0} MB/s)",
                dt_cache,
                cache_mb,
                cache_mb / dt_cache.max(0.001)
            ),
            orig_total,
            comp_total,
        );
    }

    // 5b. Parallel encode fresh (uncached) entries
    let mut errors: u32 = 0;
    let mut dt_encode = 0.0f64;
    if n_fresh > 0 {
        progress.phase_start(
            n_fresh,
            &format!(
                "Encoding {} images (q={}, s={}, w={})",
                n_fresh, quality, speed, workers
            ),
        );

        let t0 = Instant::now();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()
            .map_err(|e| format!("rayon pool: {e}"))?;

        let writer_mu = Mutex::new(writer);
        let done_count = AtomicU32::new(0);
        let err_count = AtomicU32::new(0);
        let orig_acc = AtomicU64::new(orig_total);
        let comp_acc = AtomicU64::new(comp_total);
        let manifest_mu = Mutex::new(manifest);

        pool.install(|| {
            fresh_entries.par_iter().for_each(|entry| {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }

                let result = (|| -> Result<(String, Vec<u8>, u64, u32, u32), String> {
                    let avif_name = get_avif_name(&entry.name);
                    let raw = pread_entry(src_file, entry)?;
                    let orig_bytes = raw.len() as u64;
                    let (rgba, w, h) = decode_to_rgba(&raw)?;
                    drop(raw);
                    let avif = unsafe { crate::encode_avif_raw(&rgba, w, h, quality, speed) }
                        .map_err(|c| format!("avif error {c}: {}", entry.name))?;
                    drop(rgba);

                    if let Some(cd) = cache_dir {
                        write_cache(cd, &entry.name, quality, speed, &avif);
                    }

                    Ok((avif_name, avif, orig_bytes, w, h))
                })();

                match result {
                    Ok((avif_name, avif, orig_bytes, width, height)) => {
                        let comp_bytes = avif.len() as u64;
                        let write_result = {
                            let mut w = writer_mu.lock().unwrap();
                            w.add_file(&avif_name, &avif)
                                .map_err(|e| format!("write '{}': {e}", avif_name))
                        };

                        match write_result {
                            Ok(()) => {
                                manifest_mu.lock().unwrap().insert(
                                    entry.name.clone(),
                                    ManifestAsset::avif(
                                        avif_name.clone(),
                                        Some(width),
                                        Some(height),
                                        "legacy-avif".to_string(),
                                    ),
                                );
                                let d = done_count.fetch_add(1, Ordering::Relaxed) + 1;
                                orig_acc.fetch_add(orig_bytes, Ordering::Relaxed);
                                comp_acc.fetch_add(comp_bytes, Ordering::Relaxed);
                                if d.is_multiple_of(10) || d == n_fresh {
                                    progress.task_done(
                                        d,
                                        n_fresh,
                                        &avif_name,
                                        orig_acc.load(Ordering::Relaxed),
                                        comp_acc.load(Ordering::Relaxed),
                                    );
                                }
                            }
                            Err(msg) => {
                                err_count.fetch_add(1, Ordering::Relaxed);
                                let d = done_count.fetch_add(1, Ordering::Relaxed) + 1;
                                progress.warning(&format!("[{d}/{n_fresh}] {msg}"));
                            }
                        }
                    }
                    Err(msg) => {
                        err_count.fetch_add(1, Ordering::Relaxed);
                        let d = done_count.fetch_add(1, Ordering::Relaxed) + 1;
                        progress.warning(&format!("[{d}/{n_fresh}] {msg}"));
                    }
                }
            });
        });

        dt_encode = t0.elapsed().as_secs_f64();
        orig_total = orig_acc.load(Ordering::Relaxed);
        comp_total = comp_acc.load(Ordering::Relaxed);
        errors = err_count.load(Ordering::Relaxed);
        let encoded_fresh = done_count.load(Ordering::Relaxed) - errors;

        if cancel.load(Ordering::Relaxed) {
            return Ok(BuildStats {
                total_entries: n_encode + n_pass,
                encoded: n_cached + encoded_fresh,
                passthrough: n_pass,
                original_bytes: orig_total,
                compressed_bytes: comp_total,
                encode_errors: errors,
                cache_hits: n_cached,
                cancelled: true,
                timing: BuildTiming::default(),
            });
        }

        progress.phase_end(
            n_fresh,
            &format!(
                "Encoding done ({:.1}s, {:.1} img/s)",
                dt_encode,
                n_fresh as f64 / dt_encode.max(0.001)
            ),
            orig_total,
            comp_total,
        );
        manifest = manifest_mu.into_inner().unwrap();
        writer = writer_mu.into_inner().unwrap();
    }

    // 6. Write manifest into RPA
    let t0 = Instant::now();
    progress.phase_start(1, "Writing manifest");
    let manifest_json = build_manifest_json(&manifest);
    writer
        .add_file(MANIFEST_PATH, manifest_json.as_bytes())
        .map_err(|e| format!("write manifest: {e}"))?;
    progress.phase_end(1, "Manifest written", orig_total, comp_total);

    // 7. Finalize RPA (write index)
    progress.phase_start(1, "Finalizing RPA index");
    writer.finish().map_err(|e| format!("finalize RPA: {e}"))?;
    let dt_finalize = t0.elapsed().as_secs_f64();
    progress.phase_end(
        1,
        &format!("RPA written ({:.1}s)", dt_finalize),
        orig_total,
        comp_total,
    );

    let dt_total = t_total.elapsed().as_secs_f64();
    let timing = BuildTiming {
        index_s: dt_index,
        passthrough_s: dt_pass,
        cache_s: dt_cache,
        encode_s: dt_encode,
        finalize_s: dt_finalize,
        total_s: dt_total,
    };

    let encoded = n_cached + n_fresh - errors;
    Ok(BuildStats {
        total_entries: n_encode + n_pass,
        encoded,
        passthrough: n_pass,
        original_bytes: orig_total,
        compressed_bytes: comp_total,
        encode_errors: errors,
        cache_hits: n_cached,
        cancelled: false,
        timing,
    })
}

// PLACEHOLDER_FFI

// --- Manifest generation ---

fn build_manifest_json(manifest: &Manifest) -> String {
    manifest
        .to_json_pretty()
        .unwrap_or_else(|_| "{}\n".to_string())
}

// --- FFI wrapper: adapts C callback to ProgressReport trait ---

struct CbProgress(ProgressCb);
unsafe impl Send for CbProgress {}
unsafe impl Sync for CbProgress {}

impl ProgressReport for CbProgress {
    fn phase_start(&self, total: u32, msg: &str) {
        report(self.0, 0, 0, total, msg, 0, 0);
    }
    fn task_done(&self, done: u32, total: u32, msg: &str, orig: u64, comp: u64) {
        report(self.0, 1, done, total, msg, orig, comp);
    }
    fn phase_end(&self, total: u32, msg: &str, orig: u64, comp: u64) {
        report(self.0, 2, total, total, msg, orig, comp);
    }
    fn warning(&self, msg: &str) {
        report(self.0, 3, 0, 0, msg, 0, 0);
    }
}

/// Build a compressed RPA through the C ABI.
///
/// # Safety
///
/// `input_rpa` and `output_rpa` must be valid null-terminated strings for the
/// duration of the call. `progress_cb`, when non-null, must remain callable
/// until the build returns.
#[no_mangle]
pub unsafe extern "C" fn renpak_build(
    input_rpa: *const c_char,
    output_rpa: *const c_char,
    quality: i32,
    speed: i32,
    workers: i32,
    progress_cb: ProgressCb,
) -> i32 {
    if input_rpa.is_null() || output_rpa.is_null() {
        return -1;
    }

    let input = match std::ffi::CStr::from_ptr(input_rpa).to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let output = match std::ffi::CStr::from_ptr(output_rpa).to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let w = if workers <= 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    } else {
        workers as usize
    };
    let prog = CbProgress(progress_cb);
    let no_exclude: Vec<String> = Vec::new();
    let cancel = AtomicBool::new(false);
    match build(
        Path::new(input),
        Path::new(output),
        quality,
        speed,
        w,
        &no_exclude,
        &prog,
        &cancel,
        None,
    ) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}
