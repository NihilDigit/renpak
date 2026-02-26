//! Build pipeline: parallel AVIF encoding with Rayon.
//!
//! Encode phase streams results directly into the output RPA via Mutex,
//! so memory usage stays bounded (~1 AVIF buffer per worker thread).

use std::fs::File;
use std::os::raw::c_char;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicU32, Ordering};
use std::sync::Mutex;
use std::ffi::CString;

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

const IMAGE_EXTS: &[&str] = &[".jpg", ".jpeg", ".png", ".webp", ".bmp"];
const DEFAULT_SKIP_PREFIXES: &[&str] = &["gui/"];

fn should_encode(name: &str, skip_prefixes: &[String]) -> bool {
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

// --- Image decoding ---

fn decode_to_rgba(data: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let img = image::load_from_memory(data)
        .map_err(|e| format!("decode: {e}"))?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok((rgba.into_raw(), w, h))
}

// --- pread helper ---

fn pread_entry(file: &File, entry: &RpaEntry) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; entry.length as usize];
    file.read_exact_at(&mut buf, entry.offset)
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
) -> Result<BuildStats, String> {
    // 1. Build skip prefixes: defaults + user excludes
    let mut skip_prefixes: Vec<String> = DEFAULT_SKIP_PREFIXES.iter().map(|s| s.to_string()).collect();
    skip_prefixes.extend(exclude.iter().cloned());

    // 2. Read source index
    let mut reader = RpaReader::open(input_path)
        .map_err(|e| format!("open RPA: {e}"))?;
    let index = reader.read_index()
        .map_err(|e| format!("read index: {e}"))?;
    let src_key = reader.key();

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

    // 4. Create output RPA and write passthrough entries first
    let mut writer = RpaWriter::create(output_path, src_key)
        .map_err(|e| format!("create output RPA: {e}"))?;

    progress.phase_start(n_pass, &format!("Copying {} passthrough entries", n_pass));
    let src_file = reader.file();
    for (i, entry) in to_passthrough.iter().enumerate() {
        writer.add_file_from(
            &entry.name, src_file,
            entry.offset, entry.length, &entry.prefix,
        ).map_err(|e| format!("copy '{}': {e}", entry.name))?;

        if (i + 1) % 500 == 0 || i + 1 == to_passthrough.len() {
            progress.task_done((i + 1) as u32, n_pass,
                &format!("Copied {}/{}", i + 1, n_pass), 0, 0);
        }
    }
    progress.phase_end(n_pass, "Passthrough done", 0, 0);

    // 5. Parallel encode → write immediately via Mutex<RpaWriter>
    progress.phase_start(n_encode,
        &format!("Encoding {} images (quality={}, speed={}, workers={})",
                 n_encode, quality, speed, workers));

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()
        .map_err(|e| format!("rayon pool: {e}"))?;

    let writer_mu = Mutex::new(writer);
    let done_count = AtomicU32::new(0);
    let err_count = AtomicU32::new(0);
    let orig_acc = AtomicU64::new(0);
    let comp_acc = AtomicU64::new(0);
    let manifest = Mutex::new(Vec::<(String, String)>::new());

    pool.install(|| {
        to_encode.par_iter().for_each(|entry| {
            // Read + decode + encode (no lock held)
            let result = (|| -> Result<(String, Vec<u8>, u64), String> {
                let raw = pread_entry(src_file, entry)?;
                let orig_bytes = raw.len() as u64;
                let (rgba, w, h) = decode_to_rgba(&raw)?;
                drop(raw);
                let avif = unsafe { crate::encode_avif_raw(&rgba, w, h, quality, speed) }
                    .map_err(|c| format!("avif error {c}: {}", entry.name))?;
                drop(rgba);
                let avif_name = get_avif_name(&entry.name);
                Ok((avif_name, avif, orig_bytes))
            })();

            match result {
                Ok((avif_name, avif, orig_bytes)) => {
                    let comp_bytes = avif.len() as u64;
                    // Lock, write, unlock — avif data freed after this block
                    {
                        let mut w = writer_mu.lock().unwrap();
                        let _ = w.add_file(&avif_name, &avif);
                    }
                    manifest.lock().unwrap().push((entry.name.clone(), avif_name.clone()));
                    let d = done_count.fetch_add(1, Ordering::Relaxed) + 1;
                    orig_acc.fetch_add(orig_bytes, Ordering::Relaxed);
                    comp_acc.fetch_add(comp_bytes, Ordering::Relaxed);
                    if d % 10 == 0 || d == n_encode {
                        progress.task_done(d, n_encode, &avif_name,
                            orig_acc.load(Ordering::Relaxed),
                            comp_acc.load(Ordering::Relaxed));
                    }
                }
                Err(msg) => {
                    err_count.fetch_add(1, Ordering::Relaxed);
                    let d = done_count.fetch_add(1, Ordering::Relaxed) + 1;
                    progress.warning(&format!("[{d}/{n_encode}] {msg}"));
                }
            }
        });
    });

    let orig_total = orig_acc.load(Ordering::Relaxed);
    let comp_total = comp_acc.load(Ordering::Relaxed);
    let errors = err_count.load(Ordering::Relaxed);
    let encoded = n_encode - errors;

    progress.phase_end(n_encode, "Encoding done", orig_total, comp_total);

    // 6. Write manifest into RPA
    progress.phase_start(1, "Writing manifest");
    let manifest_entries = manifest.into_inner().unwrap();
    let manifest_json = build_manifest_json(&manifest_entries);
    {
        let mut w = writer_mu.lock().unwrap();
        w.add_file("renpak_manifest.json", manifest_json.as_bytes())
            .map_err(|e| format!("write manifest: {e}"))?;
    }
    progress.phase_end(1, "Manifest written", orig_total, comp_total);

    // 7. Finalize RPA (write index)
    progress.phase_start(1, "Finalizing RPA index");
    let writer = writer_mu.into_inner().unwrap();
    writer.finish().map_err(|e| format!("finalize RPA: {e}"))?;
    progress.phase_end(1, "RPA written", orig_total, comp_total);

    Ok(BuildStats {
        total_entries: n_encode + n_pass,
        encoded,
        passthrough: n_pass,
        original_bytes: orig_total,
        compressed_bytes: comp_total,
        encode_errors: errors,
    })
}

// PLACEHOLDER_FFI

// --- Manifest generation ---

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn build_manifest_json(entries: &[(String, String)]) -> String {
    let mut json = String::with_capacity(entries.len() * 80);
    json.push('{');
    for (i, (orig, avif)) in entries.iter().enumerate() {
        if i > 0 { json.push(','); }
        json.push('\n');
        json.push_str(&format!("  \"{}\": \"{}\"", json_escape(orig), json_escape(avif)));
    }
    json.push_str("\n}\n");
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
    match build(Path::new(input), Path::new(output), quality, speed, w, &no_exclude, &prog) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}
