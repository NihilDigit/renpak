//! renpak-core: Build-time AVIS sequence encoder.
//!
//! Exports a C ABI for Python ctypes to call.
//! Links against system libavif (with rav1e encoder).

#![allow(non_camel_case_types, non_upper_case_globals)]

use std::os::raw::c_int;

// --- Constants ---

type avifResult = c_int;
const AVIF_RESULT_OK: avifResult = 0;
const AVIF_PIXEL_FORMAT_YUV444: c_int = 1;
const AVIF_RANGE_FULL: c_int = 1;
const AVIF_ADD_IMAGE_FLAG_NONE: u32 = 0;

// --- Opaque types (accessed only via C API + known offsets) ---

enum avifImage {}
enum avifEncoder {}

#[repr(C)]
struct avifRWData {
    data: *mut u8,
    size: usize,
}

// avifRGBImage is stack-allocated and initialized by avifRGBImageSetDefaults.
// We allocate it as a 64-byte zeroed buffer (sizeof(avifRGBImage) == 64 on x86_64).
// Known field offsets (verified with offsetof on libavif 1.3.0 x86_64):
//   pixels: 48, rowBytes: 56, format: 12, depth: 8
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

// avifEncoder offsets
const ENC_MAX_THREADS: usize = 4;
const ENC_SPEED: usize = 8;
const ENC_KEYFRAME_INTERVAL: usize = 12;
const ENC_TIMESCALE: usize = 16;
const ENC_QUALITY: usize = 32;
const ENC_QUALITY_ALPHA: usize = 36;

// avifImage offsets
const IMG_YUV_RANGE: usize = 16;
const IMG_COLOR_PRIMARIES: usize = 104;
const IMG_TRANSFER: usize = 106;
const IMG_MATRIX: usize = 108;

// avifRGBImage offsets
const RGB_DEPTH: usize = 8;
const RGB_FORMAT: usize = 12;
const RGB_PIXELS: usize = 48;
const RGB_ROW_BYTES: usize = 56;

// --- Public FFI ---

/// Encode multiple RGBA frames into an AVIS sequence.
#[no_mangle]
pub unsafe extern "C" fn renpak_encode_avis(
    frames_rgba: *const *const u8,
    frame_count: u32,
    width: u32,
    height: u32,
    quality: i32,
    speed: i32,
    out_data: *mut *mut u8,
    out_len: *mut usize,
) -> i32 {
    if frames_rgba.is_null() || frame_count == 0 || out_data.is_null() || out_len.is_null() {
        return -1;
    }

    let encoder = avifEncoderCreate();
    if encoder.is_null() {
        return -2;
    }
    let enc = encoder as *mut u8;

    // Configure encoder
    write_i32(enc, ENC_MAX_THREADS, 4);
    write_i32(enc, ENC_SPEED, speed.clamp(0, 10));
    // Star GOP: keyframeInterval = frame_count means all frames reference the first I-frame
    write_i32(enc, ENC_KEYFRAME_INTERVAL, frame_count as i32);
    write_u64(enc, ENC_TIMESCALE, 1);
    write_i32(enc, ENC_QUALITY, quality.clamp(0, 100));
    write_i32(enc, ENC_QUALITY_ALPHA, quality.clamp(0, 100));

    let mut output = avifRWData { data: std::ptr::null_mut(), size: 0 };

    for i in 0..frame_count {
        let frame_ptr = *frames_rgba.add(i as usize);
        if frame_ptr.is_null() {
            avifEncoderDestroy(encoder);
            return -3;
        }

        let image = avifImageCreate(width, height, 8, AVIF_PIXEL_FORMAT_YUV444);
        if image.is_null() {
            avifEncoderDestroy(encoder);
            return -4;
        }
        let img = image as *mut u8;

        // Set CICP + Full Range
        write_i32(img, IMG_YUV_RANGE, AVIF_RANGE_FULL);
        write_u16(img, IMG_COLOR_PRIMARIES, 1);   // BT709
        write_u16(img, IMG_TRANSFER, 13);          // sRGB
        write_u16(img, IMG_MATRIX, 1);             // BT709

        // Prepare RGB source
        let mut rgb_buf = [0u8; SIZEOF_AVIF_RGB_IMAGE];
        let rgb = rgb_buf.as_mut_ptr();
        avifRGBImageSetDefaults(rgb, image);
        // Override format to RGBA, depth 8, and point to caller's buffer
        write_u32(rgb, RGB_DEPTH, 8);
        write_i32(rgb, RGB_FORMAT, 1); // AVIF_RGB_FORMAT_RGBA
        write_ptr(rgb, RGB_PIXELS, frame_ptr as *mut u8);
        write_u32(rgb, RGB_ROW_BYTES, width * 4);

        // Convert RGBA → YUV
        let r = avifImageRGBToYUV(image, rgb);
        if r != AVIF_RESULT_OK {
            avifImageDestroy(image);
            avifEncoderDestroy(encoder);
            return -5;
        }

        let r = avifEncoderAddImage(encoder, image, 1, AVIF_ADD_IMAGE_FLAG_NONE);
        avifImageDestroy(image);
        if r != AVIF_RESULT_OK {
            avifEncoderDestroy(encoder);
            return -6;
        }
    }

    let r = avifEncoderFinish(encoder, &mut output);
    avifEncoderDestroy(encoder);
    if r != AVIF_RESULT_OK {
        avifRWDataFree(&mut output);
        return -7;
    }

    // Copy to Rust-allocated buffer (so renpak_free can dealloc it)
    let len = output.size;
    if len == 0 {
        avifRWDataFree(&mut output);
        return -9;
    }
    let layout = std::alloc::Layout::from_size_align(len, 1).unwrap();
    let buf = std::alloc::alloc(layout);
    if buf.is_null() {
        avifRWDataFree(&mut output);
        return -8;
    }
    std::ptr::copy_nonoverlapping(output.data, buf, len);
    avifRWDataFree(&mut output);

    *out_data = buf;
    *out_len = len;
    0
}

/// Free a buffer allocated by renpak functions.
#[no_mangle]
pub unsafe extern "C" fn renpak_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len > 0 {
        let layout = std::alloc::Layout::from_size_align(len, 1).unwrap();
        std::alloc::dealloc(ptr, layout);
    }
}
