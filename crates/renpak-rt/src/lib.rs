//! renpak-rt: Runtime AVIS sequence decoder.
//!
//! Exports a C ABI for Python ctypes to call inside Ren'Py.
//! Links against system libavif (with dav1d decoder).
//! Decodes a specific frame from an AVIS byte stream and returns PNG bytes.

#![allow(non_camel_case_types, dead_code)]

use std::os::raw::c_int;

type avifResult = c_int;
const AVIF_RESULT_OK: avifResult = 0;

// Opaque types
enum avifDecoder {}
enum avifImage {}

#[repr(C)]
struct avifRWData {
    data: *mut u8,
    size: usize,
}

const SIZEOF_AVIF_RGB_IMAGE: usize = 64;

extern "C" {
    fn avifDecoderCreate() -> *mut avifDecoder;
    fn avifDecoderDestroy(dec: *mut avifDecoder);
    fn avifDecoderSetIOMemory(dec: *mut avifDecoder, data: *const u8, size: usize) -> avifResult;
    fn avifDecoderParse(dec: *mut avifDecoder) -> avifResult;
    fn avifDecoderNthImage(dec: *mut avifDecoder, idx: u32) -> avifResult;
    fn avifRGBImageSetDefaults(rgb: *mut u8, image: *const avifImage);
    fn avifRGBImageAllocatePixels(rgb: *mut u8) -> avifResult;
    fn avifRGBImageFreePixels(rgb: *mut u8);
    fn avifImageYUVToRGB(image: *const avifImage, rgb: *mut u8) -> avifResult;
}

// avifDecoder field offsets (libavif 1.3.0, x86_64)
const DEC_IMAGE: usize = 48;      // avifImage* image
const DEC_IMAGE_INDEX: usize = 56; // int imageIndex
const DEC_IMAGE_COUNT: usize = 60; // int imageCount

// avifImage field offsets
const IMG_WIDTH: usize = 0;
const IMG_HEIGHT: usize = 4;

// avifRGBImage field offsets
const RGB_WIDTH: usize = 0;
const RGB_HEIGHT: usize = 4;
const RGB_DEPTH: usize = 8;
const RGB_FORMAT: usize = 12;
const RGB_PIXELS: usize = 48;
const RGB_ROW_BYTES: usize = 56;

// PLACEHOLDER_CONTINUED

unsafe fn read_u32(base: *const u8, off: usize) -> u32 {
    (base.add(off) as *const u32).read()
}
unsafe fn read_i32(base: *const u8, off: usize) -> i32 {
    (base.add(off) as *const i32).read()
}
unsafe fn read_ptr(base: *const u8, off: usize) -> *const u8 {
    (base.add(off) as *const *const u8).read()
}

/// Encode RGBA pixels to PNG bytes.
fn rgba_to_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, png::EncodingError> {
    let mut buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut buf, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(rgba)?;
    }
    Ok(buf)
}

/// Decode a specific frame from AVIS bytes and return PNG bytes.
#[no_mangle]
pub unsafe extern "C" fn renpak_decode_frame_png(
    avis_data: *const u8,
    avis_len: usize,
    frame_index: u32,
    out_png: *mut *mut u8,
    out_png_len: *mut usize,
) -> i32 {
    if avis_data.is_null() || avis_len == 0 || out_png.is_null() || out_png_len.is_null() {
        return -1;
    }

    let decoder = avifDecoderCreate();
    if decoder.is_null() {
        return -2;
    }

    let r = avifDecoderSetIOMemory(decoder, avis_data, avis_len);
    if r != AVIF_RESULT_OK {
        avifDecoderDestroy(decoder);
        return -3;
    }

    let r = avifDecoderParse(decoder);
    if r != AVIF_RESULT_OK {
        avifDecoderDestroy(decoder);
        return -4;
    }

    let dec = decoder as *const u8;
    let image_count = read_i32(dec, DEC_IMAGE_COUNT);
    if frame_index >= image_count as u32 {
        avifDecoderDestroy(decoder);
        return -5;
    }

    let r = avifDecoderNthImage(decoder, frame_index);
    if r != AVIF_RESULT_OK {
        avifDecoderDestroy(decoder);
        return -6;
    }

    // Get decoded image pointer from decoder->image
    let image = read_ptr(dec, DEC_IMAGE) as *const avifImage;
    if image.is_null() {
        avifDecoderDestroy(decoder);
        return -7;
    }

    let img = image as *const u8;
    let width = read_u32(img, IMG_WIDTH);
    let height = read_u32(img, IMG_HEIGHT);

    // Convert YUV → RGBA
    let mut rgb_buf = [0u8; SIZEOF_AVIF_RGB_IMAGE];
    let rgb = rgb_buf.as_mut_ptr();
    avifRGBImageSetDefaults(rgb, image);
    // Defaults set RGBA format, 8-bit depth — that's what we want

    let r = avifRGBImageAllocatePixels(rgb);
    if r != AVIF_RESULT_OK {
        avifDecoderDestroy(decoder);
        return -8;
    }

    let r = avifImageYUVToRGB(image, rgb);
    if r != AVIF_RESULT_OK {
        avifRGBImageFreePixels(rgb);
        avifDecoderDestroy(decoder);
        return -9;
    }

    // Read RGBA pixels
    let pixels_ptr = read_ptr(rgb, RGB_PIXELS);
    let row_bytes = read_u32(rgb, RGB_ROW_BYTES);
    let rgba_size = (row_bytes * height) as usize;
    let rgba_slice = std::slice::from_raw_parts(pixels_ptr, rgba_size);

    // If rowBytes == width*4, we can use the slice directly.
    // Otherwise we need to strip padding.
    let rgba_data = if row_bytes == width * 4 {
        rgba_slice.to_vec()
    } else {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            let row_start = (y * row_bytes) as usize;
            let row_end = row_start + (width * 4) as usize;
            data.extend_from_slice(&rgba_slice[row_start..row_end]);
        }
        data
    };

    avifRGBImageFreePixels(rgb);
    avifDecoderDestroy(decoder);

    // Encode to PNG
    let png_bytes = match rgba_to_png(&rgba_data, width, height) {
        Ok(b) => b,
        Err(_) => return -10,
    };

    // Allocate output buffer
    let len = png_bytes.len();
    let layout = std::alloc::Layout::from_size_align(len, 1).unwrap();
    let buf = std::alloc::alloc(layout);
    if buf.is_null() {
        return -11;
    }
    std::ptr::copy_nonoverlapping(png_bytes.as_ptr(), buf, len);

    *out_png = buf;
    *out_png_len = len;
    0
}

/// Query AVIS frame count and dimensions.
#[no_mangle]
pub unsafe extern "C" fn renpak_avis_info(
    avis_data: *const u8,
    avis_len: usize,
    out_frame_count: *mut u32,
    out_width: *mut u32,
    out_height: *mut u32,
) -> i32 {
    if avis_data.is_null() || avis_len == 0 {
        return -1;
    }

    let decoder = avifDecoderCreate();
    if decoder.is_null() {
        return -2;
    }

    let r = avifDecoderSetIOMemory(decoder, avis_data, avis_len);
    if r != AVIF_RESULT_OK {
        avifDecoderDestroy(decoder);
        return -3;
    }

    let r = avifDecoderParse(decoder);
    if r != AVIF_RESULT_OK {
        avifDecoderDestroy(decoder);
        return -4;
    }

    let dec = decoder as *const u8;
    let image = read_ptr(dec, DEC_IMAGE) as *const u8;

    if !out_frame_count.is_null() {
        *out_frame_count = read_i32(dec, DEC_IMAGE_COUNT) as u32;
    }
    if !out_width.is_null() && !image.is_null() {
        *out_width = read_u32(image, IMG_WIDTH);
    }
    if !out_height.is_null() && !image.is_null() {
        *out_height = read_u32(image, IMG_HEIGHT);
    }

    avifDecoderDestroy(decoder);
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
