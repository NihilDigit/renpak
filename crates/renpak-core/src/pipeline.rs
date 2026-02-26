//! Build pipeline: parallel AVIF encoding with Rayon.
//!
//! Encode phase streams results directly into the output RPA via Mutex,
//! so memory usage stays bounded (~1 AVIF buffer per worker thread).

use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io;
use std::os::raw::c_char;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU32, Ordering};
use std::sync::Mutex;
use std::ffi::CString;
use std::time::Instant;

#[cfg(unix)]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;

use rayon::prelude::*;

use crate::rpa::{RpaReader, RpaWriter, RpaEntry};

// --- Progress callback (C ABI, kept for FFI) ---

#[repr(C)]
pub struct ProgressEvent {
    pub kind: u32,       // 0=phase_start, 1=task_done, 2=phase_end, 3=warning
    pub done: u32,
    pub total: u32,
    pub message: *const c_char,
    pub original_bytes: u64,
    pub compressed_bytes: u64,
}

pub type ProgressCb = Option<unsafe extern "C" fn(*const ProgressEvent)>;

fn report(cb: ProgressCb, kind: u32, done: u32, total: u32, msg: &str,
          orig: u64, comp: u64) {
    if let Some(f) = cb {
        let c = CString::new(msg).unwrap_or_default();
        let ev = ProgressEvent {
            kind, done, total,
            message: c.as_ptr(),
            original_bytes: orig,
            compressed_bytes: comp,
        };
        unsafe { f(&ev); }
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
pub const DEFAULT_SKIP_PREFIXES: &[&str] = &["gui/"];

pub fn should_encode(name: &str, skip_prefixes: &[String]) -> bool {
    let lower = name.to_ascii_lowercase();
    let is_img = IMAGE_EXTS.iter().any(|e| lower.ends_with(e));
    let skip = skip_prefixes.iter().any(|p| lower.starts_with(p.as_str()));
    is_img && !skip
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
    let img = image::load_from_memory(data)
        .map_err(|e| format!("decode: {e}"))?;
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
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "unexpected eof"));
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

// PLACEHOLDER_BUILD_MAIN

/// Build a compressed RPA: read source → passthrough copy → parallel AVIF encode + write.
///
/// Encoded AVIF data is written to the output RPA immediately via Mutex,
/// keeping memory usage bounded to ~1 buffer per worker thread.
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
    let mut skip_prefixes: Vec<String> = DEFAULT_SKIP_PREFIXES.iter().map(|s| s.to_string()).collect();
    skip_prefixes.extend(exclude.iter().cloned());

    // 2. Read source index
    let t0 = Instant::now();
    let mut reader = RpaReader::open(input_path)
        .map_err(|e| format!("open RPA: {e}"))?;
    let index = reader.read_index()
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
    let mut writer = RpaWriter::create(output_path, src_key)
        .map_err(|e| format!("create output RPA: {e}"))?;

    progress.phase_start(n_pass, &format!("Copying {} passthrough entries", n_pass));
    let src_file = reader.file();
    let mut copy_buf = vec![0u8; 1024 * 1024]; // 1MB reusable buffer
    let t0 = Instant::now();
    for (i, entry) in to_passthrough.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(BuildStats {
                total_entries: n_encode + n_pass, encoded: 0,
                passthrough: i as u32, original_bytes: 0,
                compressed_bytes: 0, encode_errors: 0, cache_hits: 0, cancelled: true,
                timing: BuildTiming::default(),
            });
        }
        writer.add_file_from(
            &entry.name, src_file,
            entry.offset, entry.length, &entry.prefix, &mut copy_buf,
        ).map_err(|e| format!("copy '{}': {e}", entry.name))?;

        if (i + 1) % 500 == 0 || i + 1 == to_passthrough.len() {
            progress.task_done((i + 1) as u32, n_pass,
                &format!("Copied {}/{}", i + 1, n_pass), 0, 0);
        }
    }
    let dt_pass = t0.elapsed().as_secs_f64();
    let pass_mb = to_passthrough.iter().map(|e| e.length + e.prefix.len() as u64).sum::<u64>() as f64 / 1_048_576.0;
    progress.phase_end(n_pass, &format!("Passthrough done ({:.1}s, {:.0} MB, {:.0} MB/s)",
        dt_pass, pass_mb, pass_mb / dt_pass.max(0.001)), 0, 0);

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

    let mut manifest_entries: Vec<(String, String)> = Vec::new();
    let mut orig_total: u64 = 0;
    let mut comp_total: u64 = 0;

    // 5a. Restore cached entries (sequential, fast I/O only)
    let mut dt_cache = 0.0f64;
    if n_cached > 0 {
        progress.phase_start(n_cached,
            &format!("Restoring {} cached images", n_cached));
        let t0 = Instant::now();
        for (i, entry) in cached_entries.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(BuildStats {
                    total_entries: n_encode + n_pass, encoded: i as u32,
                    passthrough: n_pass, original_bytes: orig_total,
                    compressed_bytes: comp_total, encode_errors: 0,
                    cache_hits: i as u32, cancelled: true,
                    timing: BuildTiming::default(),
                });
            }
            let avif_name = get_avif_name(&entry.name);
            let cached = read_cache(cache_dir.unwrap(), &entry.name, quality, speed)
                .ok_or_else(|| format!("cache miss for {}", entry.name))?;
            let orig_bytes = entry.length + entry.prefix.len() as u64;
            let comp_bytes = cached.len() as u64;
            writer.add_file(&avif_name, &cached)
                .map_err(|e| format!("write cached '{}': {e}", avif_name))?;
            manifest_entries.push((entry.name.clone(), avif_name.clone()));
            orig_total += orig_bytes;
            comp_total += comp_bytes;
            if (i + 1) % 100 == 0 || i + 1 == cached_entries.len() {
                progress.task_done((i + 1) as u32, n_cached, &avif_name,
                    orig_total, comp_total);
            }
        }
        dt_cache = t0.elapsed().as_secs_f64();
        let cache_mb = comp_total as f64 / 1_048_576.0;
        progress.phase_end(n_cached, &format!("Cache restored ({:.1}s, {:.0} MB, {:.0} MB/s)",
            dt_cache, cache_mb, cache_mb / dt_cache.max(0.001)), orig_total, comp_total);
    }

    // 5b. Parallel encode fresh (uncached) entries
    let mut errors: u32 = 0;
    let mut dt_encode = 0.0f64;
    if n_fresh > 0 {
        progress.phase_start(n_fresh,
            &format!("Encoding {} images (q={}, s={}, w={})",
                     n_fresh, quality, speed, workers));

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
        let manifest_mu = Mutex::new(manifest_entries);

        pool.install(|| {
            fresh_entries.par_iter().for_each(|entry| {
                if cancel.load(Ordering::Relaxed) { return; }

                let result = (|| -> Result<(String, Vec<u8>, u64), String> {
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

                    Ok((avif_name, avif, orig_bytes))
                })();

                match result {
                    Ok((avif_name, avif, orig_bytes)) => {
                        let comp_bytes = avif.len() as u64;
                        let write_result = {
                            let mut w = writer_mu.lock().unwrap();
                            w.add_file(&avif_name, &avif)
                                .map_err(|e| format!("write '{}': {e}", avif_name))
                        };

                        match write_result {
                            Ok(()) => {
                                manifest_mu.lock().unwrap().push((entry.name.clone(), avif_name.clone()));
                                let d = done_count.fetch_add(1, Ordering::Relaxed) + 1;
                                orig_acc.fetch_add(orig_bytes, Ordering::Relaxed);
                                comp_acc.fetch_add(comp_bytes, Ordering::Relaxed);
                                if d % 10 == 0 || d == n_fresh {
                                    progress.task_done(d, n_fresh, &avif_name,
                                        orig_acc.load(Ordering::Relaxed),
                                        comp_acc.load(Ordering::Relaxed));
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
                original_bytes: orig_total, compressed_bytes: comp_total,
                encode_errors: errors, cache_hits: n_cached, cancelled: true,
                timing: BuildTiming::default(),
            });
        }

        progress.phase_end(n_fresh, &format!("Encoding done ({:.1}s, {:.1} img/s)",
            dt_encode, n_fresh as f64 / dt_encode.max(0.001)), orig_total, comp_total);
        manifest_entries = manifest_mu.into_inner().unwrap();
        writer = writer_mu.into_inner().unwrap();
    }

    // 6. Write manifest into RPA
    let t0 = Instant::now();
    progress.phase_start(1, "Writing manifest");
    let manifest_json = build_manifest_json(&manifest_entries);
    writer.add_file("renpak_manifest.json", manifest_json.as_bytes())
        .map_err(|e| format!("write manifest: {e}"))?;
    progress.phase_end(1, "Manifest written", orig_total, comp_total);

    // 7. Finalize RPA (write index)
    progress.phase_start(1, "Finalizing RPA index");
    writer.finish().map_err(|e| format!("finalize RPA: {e}"))?;
    let dt_finalize = t0.elapsed().as_secs_f64();
    progress.phase_end(1, &format!("RPA written ({:.1}s)", dt_finalize), orig_total, comp_total);

    let dt_total = t_total.elapsed().as_secs_f64();
    let timing = BuildTiming {
        index_s: dt_index, passthrough_s: dt_pass, cache_s: dt_cache,
        encode_s: dt_encode, finalize_s: dt_finalize, total_s: dt_total,
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

fn build_manifest_json(entries: &[(String, String)]) -> String {
    let mut map = std::collections::BTreeMap::new();
    for (orig, avif) in entries {
        map.insert(orig.clone(), avif.clone());
    }

    let mut json = serde_json::to_string_pretty(&map).unwrap_or_else(|_| "{}".to_string());
    json.push('\n');
    json
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
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)
    } else {
        workers as usize
    };
    let prog = CbProgress(progress_cb);
    let no_exclude: Vec<String> = Vec::new();
    let cancel = AtomicBool::new(false);
    match build(Path::new(input), Path::new(output), quality, speed, w, &no_exclude, &prog, &cancel, None) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}
