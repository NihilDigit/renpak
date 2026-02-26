//! renpak-core: Build-time encoder and build engine.
//!
//! Exports C ABI for Python ctypes.
//! Links against system libavif (with aom encoder).

#![allow(non_camel_case_types, non_upper_case_globals)]

pub mod rpa;
pub mod pipeline;
pub mod tui;

// Re-export for tests
pub use rpa::{RpaReader, RpaWriter, RpaEntry};

use std::os::raw::c_int;

// --- libavif constants and FFI (unchanged from Phase 2) ---

type avifResult = c_int;
const AVIF_RESULT_OK: avifResult = 0;
const AVIF_PIXEL_FORMAT_YUV444: c_int = 1;
const AVIF_RANGE_FULL: c_int = 1;
const AVIF_ADD_IMAGE_FLAG_NONE: u32 = 0;

enum avifImage {}
enum avifEncoder {}

#[repr(C)]
struct avifRWData {
    data: *mut u8,
    size: usize,
}

const SIZEOF_AVIF_RGB_IMAGE: usize = 64;

extern "C" {
    fn avifImageCreate(w: u32, h: u32, depth: u32, fmt: c_int) -> *mut avifImage;
    fn avifImageDestroy(image: *mut avifImage);
    fn avifRGBImageSetDefaults(rgb: *mut u8, image: *const avifImage);
    fn avifImageRGBToYUV(image: *mut avifImage, rgb: *const u8) -> avifResult;
    fn avifEncoderCreate() -> *mut avifEncoder;
    fn avifEncoderDestroy(encoder: *mut avifEncoder);
    fn avifEncoderAddImage(
        enc: *mut avifEncoder, img: *const avifImage, dur: u64, flags: u32,
    ) -> avifResult;
    fn avifEncoderFinish(enc: *mut avifEncoder, out: *mut avifRWData) -> avifResult;
    fn avifRWDataFree(raw: *mut avifRWData);
}

// --- Field access helpers ---
// All offsets verified via offsetof() on libavif 1.3.0, x86_64 Linux.

unsafe fn write_i32(base: *mut u8, off: usize, val: i32) {
    (base.add(off) as *mut i32).write(val);
}
unsafe fn write_u16(base: *mut u8, off: usize, val: u16) {
    (base.add(off) as *mut u16).write(val);
}
unsafe fn write_u32(base: *mut u8, off: usize, val: u32) {
    (base.add(off) as *mut u32).write(val);
}
unsafe fn write_u64(base: *mut u8, off: usize, val: u64) {
    (base.add(off) as *mut u64).write(val);
}
unsafe fn write_ptr(base: *mut u8, off: usize, val: *mut u8) {
    (base.add(off) as *mut *mut u8).write(val);
}

const ENC_MAX_THREADS: usize = 4;
const ENC_SPEED: usize = 8;
const ENC_KEYFRAME_INTERVAL: usize = 12;
const ENC_TIMESCALE: usize = 16;
const ENC_QUALITY: usize = 32;
const ENC_QUALITY_ALPHA: usize = 36;

const IMG_YUV_RANGE: usize = 16;
const IMG_COLOR_PRIMARIES: usize = 104;
const IMG_TRANSFER: usize = 106;
const IMG_MATRIX: usize = 108;

const RGB_DEPTH: usize = 8;
const RGB_FORMAT: usize = 12;
const RGB_PIXELS: usize = 48;
const RGB_ROW_BYTES: usize = 56;

/// Encode a single RGBA image to AVIF. Returns AVIF bytes.
///
/// This is the Rust-native API (not FFI). Used by the build pipeline.
pub unsafe fn encode_avif_raw(
    rgba: &[u8], width: u32, height: u32, quality: i32, speed: i32,
) -> Result<Vec<u8>, i32> {
    let encoder = avifEncoderCreate();
    if encoder.is_null() { return Err(-2); }
    let enc = encoder as *mut u8;

    write_i32(enc, ENC_MAX_THREADS, 1);
    write_i32(enc, ENC_SPEED, speed.clamp(0, 10));
    write_i32(enc, ENC_KEYFRAME_INTERVAL, 0);
    write_u64(enc, ENC_TIMESCALE, 1);
    write_i32(enc, ENC_QUALITY, quality.clamp(0, 100));
    write_i32(enc, ENC_QUALITY_ALPHA, quality.clamp(0, 100));

    let image = avifImageCreate(width, height, 8, AVIF_PIXEL_FORMAT_YUV444);
    if image.is_null() { avifEncoderDestroy(encoder); return Err(-4); }
    let img = image as *mut u8;

    write_i32(img, IMG_YUV_RANGE, AVIF_RANGE_FULL);
    write_u16(img, IMG_COLOR_PRIMARIES, 1);
    write_u16(img, IMG_TRANSFER, 13);
    write_u16(img, IMG_MATRIX, 1);

    let mut rgb_buf = [0u8; SIZEOF_AVIF_RGB_IMAGE];
    let rgb = rgb_buf.as_mut_ptr();
    avifRGBImageSetDefaults(rgb, image);
    write_u32(rgb, RGB_DEPTH, 8);
    write_i32(rgb, RGB_FORMAT, 1);
    write_ptr(rgb, RGB_PIXELS, rgba.as_ptr() as *mut u8);
    write_u32(rgb, RGB_ROW_BYTES, width * 4);

    let r = avifImageRGBToYUV(image, rgb);
    if r != AVIF_RESULT_OK {
        avifImageDestroy(image);
        avifEncoderDestroy(encoder);
        return Err(-5);
    }

    let r = avifEncoderAddImage(encoder, image, 1, AVIF_ADD_IMAGE_FLAG_NONE);
    avifImageDestroy(image);
    if r != AVIF_RESULT_OK { avifEncoderDestroy(encoder); return Err(-6); }

    let mut output = avifRWData { data: std::ptr::null_mut(), size: 0 };
    let r = avifEncoderFinish(encoder, &mut output);
    avifEncoderDestroy(encoder);
    if r != AVIF_RESULT_OK { avifRWDataFree(&mut output); return Err(-7); }

    let result = std::slice::from_raw_parts(output.data, output.size).to_vec();
    avifRWDataFree(&mut output);
    Ok(result)
}

/// Encode RGBA frames into AVIS (streaming: one frame at a time).
pub unsafe fn encode_avis_streaming(
    frames: impl Iterator<Item = (Vec<u8>, u32, u32)>,
    frame_count: u32,
    quality: i32,
    speed: i32,
) -> Result<Vec<u8>, i32> {
    let encoder = avifEncoderCreate();
    if encoder.is_null() { return Err(-2); }
    let enc = encoder as *mut u8;

    write_i32(enc, ENC_MAX_THREADS, 1);
    write_i32(enc, ENC_SPEED, speed.clamp(0, 10));
    write_i32(enc, ENC_KEYFRAME_INTERVAL, frame_count as i32);
    write_u64(enc, ENC_TIMESCALE, 1);
    write_i32(enc, ENC_QUALITY, quality.clamp(0, 100));
    write_i32(enc, ENC_QUALITY_ALPHA, quality.clamp(0, 100));

    let mut output = avifRWData { data: std::ptr::null_mut(), size: 0 };

    for (rgba, width, height) in frames {
        let image = avifImageCreate(width, height, 8, AVIF_PIXEL_FORMAT_YUV444);
        if image.is_null() { avifEncoderDestroy(encoder); return Err(-4); }
        let img = image as *mut u8;

        write_i32(img, IMG_YUV_RANGE, AVIF_RANGE_FULL);
        write_u16(img, IMG_COLOR_PRIMARIES, 1);
        write_u16(img, IMG_TRANSFER, 13);
        write_u16(img, IMG_MATRIX, 1);

        let mut rgb_buf = [0u8; SIZEOF_AVIF_RGB_IMAGE];
        let rgb = rgb_buf.as_mut_ptr();
        avifRGBImageSetDefaults(rgb, image);
        write_u32(rgb, RGB_DEPTH, 8);
        write_i32(rgb, RGB_FORMAT, 1);
        write_ptr(rgb, RGB_PIXELS, rgba.as_ptr() as *mut u8);
        write_u32(rgb, RGB_ROW_BYTES, width * 4);

        let r = avifImageRGBToYUV(image, rgb);
        if r != AVIF_RESULT_OK {
            avifImageDestroy(image);
            avifEncoderDestroy(encoder);
            return Err(-5);
        }

        let r = avifEncoderAddImage(encoder, image, 1, AVIF_ADD_IMAGE_FLAG_NONE);
        avifImageDestroy(image);
        if r != AVIF_RESULT_OK { avifEncoderDestroy(encoder); return Err(-6); }
        // rgba is dropped here — memory freed immediately
    }

    let r = avifEncoderFinish(encoder, &mut output);
    avifEncoderDestroy(encoder);
    if r != AVIF_RESULT_OK { avifRWDataFree(&mut output); return Err(-7); }

    let result = std::slice::from_raw_parts(output.data, output.size).to_vec();
    avifRWDataFree(&mut output);
    Ok(result)
}

// --- Legacy FFI (kept for backward compat with Python ctypes) ---

#[no_mangle]
pub unsafe extern "C" fn renpak_encode_avis(
    frames_rgba: *const *const u8, frame_count: u32,
    width: u32, height: u32, quality: i32, speed: i32,
    out_data: *mut *mut u8, out_len: *mut usize,
) -> i32 {
    if frames_rgba.is_null() || frame_count == 0 || out_data.is_null() || out_len.is_null() {
        return -1;
    }
    // Wrap raw pointers as an iterator of borrowed slices
    let frame_size = (width * height * 4) as usize;
    let frames = (0..frame_count).map(|i| {
        let ptr = *frames_rgba.add(i as usize);
        let slice = std::slice::from_raw_parts(ptr, frame_size);
        (slice.to_vec(), width, height)
    });

    match encode_avis_streaming(frames, frame_count, quality, speed) {
        Ok(data) => {
            let len = data.len();
            let layout = std::alloc::Layout::from_size_align(len, 1).unwrap();
            let buf = std::alloc::alloc(layout);
            if buf.is_null() { return -8; }
            std::ptr::copy_nonoverlapping(data.as_ptr(), buf, len);
            *out_data = buf;
            *out_len = len;
            0
        }
        Err(code) => code,
    }
}

#[no_mangle]
pub unsafe extern "C" fn renpak_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len > 0 {
        let layout = std::alloc::Layout::from_size_align(len, 1).unwrap();
        std::alloc::dealloc(ptr, layout);
    }
}
