use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

use image::ImageEncoder;
use renpak_core::pipeline::{self, NoProgress, Vp9BuildOptions, Vp9PlanMode};
use renpak_core::{RpaReader, RpaWriter};

#[test]
fn test_plan_vp9_reports_candidate_filters_and_bundles() {
    let root = test_root();
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let input = root.join("input-plan.rpa");
    let mut writer = RpaWriter::create(&input, 0).unwrap();
    writer
        .add_file(
            "images/large-a.png",
            &png_bytes_sized([255, 0, 0, 255], 80, 48),
        )
        .unwrap();
    writer
        .add_file(
            "images/large-b.png",
            &png_bytes_sized([0, 255, 0, 255], 80, 48),
        )
        .unwrap();
    writer
        .add_file(
            "images/small.png",
            &png_bytes_sized([0, 0, 255, 255], 16, 16),
        )
        .unwrap();
    writer
        .add_file(
            "images/transparent.png",
            &png_bytes_sized([255, 255, 255, 128], 80, 48),
        )
        .unwrap();
    writer
        .add_file(
            "gui/button.png",
            &png_bytes_sized([255, 0, 255, 255], 80, 48),
        )
        .unwrap();
    writer.add_file("images/broken.png", b"not a png").unwrap();
    writer
        .add_file("script.rpy", b"label start:\n    return\n")
        .unwrap();
    writer.finish().unwrap();

    let options = Vp9BuildOptions {
        limit: 10,
        video_limit: 0,
        bundle_size: 2,
        min_width: 64,
        min_height: 32,
        skip_transparent: true,
        min_savings_percent: 8,
        strip_optimized: false,
        optimize_videos: false,
    };
    let cancel = AtomicBool::new(false);
    let stats = pipeline::plan_vp9_with_mode(
        &input,
        &options,
        &[],
        Vp9PlanMode::Exact,
        &NoProgress,
        &cancel,
    )
    .unwrap();

    assert_eq!(stats.total_entries, 7);
    assert_eq!(stats.image_entries, 6);
    assert_eq!(stats.excluded_image_entries, 1);
    assert_eq!(stats.inspected_image_entries, 4);
    assert_eq!(stats.selected_frames, 2);
    assert_eq!(stats.planned_bundles_before_size_guard, 1);
    assert_eq!(stats.planned_bundles_grouped_by_dimension, 1);
    assert_eq!(stats.skipped_small, 1);
    assert_eq!(stats.skipped_transparent, 1);
    assert_eq!(stats.transparency_unchecked, 0);
    assert_eq!(stats.decode_errors, 1);
    assert!(!stats.limit_reached);
    assert_eq!(stats.dimensions.len(), 1);
    assert_eq!(stats.dimensions[0].width, 80);
    assert_eq!(stats.dimensions[0].height, 48);
    assert_eq!(stats.dimensions[0].frames, 2);
    assert_eq!(stats.dimensions[0].planned_bundles, 1);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_plan_vp9_fast_uses_headers_without_transparency_filter() {
    let root = test_root();
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let input = root.join("input-plan-fast.rpa");
    let mut writer = RpaWriter::create(&input, 0).unwrap();
    writer
        .add_file(
            "images/large-a.png",
            &png_bytes_sized([255, 0, 0, 255], 80, 48),
        )
        .unwrap();
    writer
        .add_file(
            "images/transparent.png",
            &png_bytes_sized([255, 255, 255, 128], 80, 48),
        )
        .unwrap();
    writer
        .add_file(
            "images/small.png",
            &png_bytes_sized([0, 0, 255, 255], 16, 16),
        )
        .unwrap();
    writer.add_file("images/broken.png", b"not a png").unwrap();
    writer.finish().unwrap();

    let options = Vp9BuildOptions {
        limit: 10,
        video_limit: 0,
        bundle_size: 2,
        min_width: 64,
        min_height: 32,
        skip_transparent: true,
        min_savings_percent: 8,
        strip_optimized: false,
        optimize_videos: false,
    };
    let cancel = AtomicBool::new(false);
    let stats = pipeline::plan_vp9(&input, &options, &[], &NoProgress, &cancel).unwrap();

    assert_eq!(stats.image_entries, 4);
    assert_eq!(stats.inspected_image_entries, 3);
    assert_eq!(stats.selected_frames, 2);
    assert_eq!(stats.planned_bundles_before_size_guard, 1);
    assert_eq!(stats.planned_bundles_grouped_by_dimension, 1);
    assert_eq!(stats.skipped_small, 1);
    assert_eq!(stats.skipped_transparent, 0);
    assert_eq!(stats.transparency_unchecked, 2);
    assert_eq!(stats.decode_errors, 1);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_plan_vp9_limit_follows_build_candidate_order() {
    let root = test_root();
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let input = root.join("input-plan-order.rpa");
    let mut writer = RpaWriter::create(&input, 0).unwrap();
    writer
        .add_file(
            "images/minority-a.png",
            &png_bytes_sized([255, 0, 0, 255], 90, 48),
        )
        .unwrap();
    writer
        .add_file(
            "images/majority-a.png",
            &png_bytes_sized([0, 255, 0, 255], 80, 48),
        )
        .unwrap();
    writer
        .add_file(
            "images/majority-b.png",
            &png_bytes_sized([0, 0, 255, 255], 80, 48),
        )
        .unwrap();
    writer
        .add_file(
            "images/majority-c.png",
            &png_bytes_sized([255, 255, 0, 255], 80, 48),
        )
        .unwrap();
    writer.finish().unwrap();

    let options = Vp9BuildOptions {
        limit: 2,
        video_limit: 0,
        bundle_size: 2,
        min_width: 64,
        min_height: 32,
        skip_transparent: false,
        min_savings_percent: 8,
        strip_optimized: false,
        optimize_videos: false,
    };
    let cancel = AtomicBool::new(false);
    let stats = pipeline::plan_vp9(&input, &options, &[], &NoProgress, &cancel).unwrap();

    assert_eq!(stats.selected_frames, 2);
    assert!(stats.limit_reached);
    assert_eq!(stats.dimensions.len(), 1);
    assert_eq!(stats.dimensions[0].width, 80);
    assert_eq!(stats.dimensions[0].height, 48);
    assert_eq!(stats.planned_bundles_before_size_guard, 1);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_plan_vp9_uses_natural_path_order_inside_dimension_group() {
    let root = test_root();
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let input = root.join("input-plan-natural-order.rpa");
    let frame_one = png_bytes_sized([255, 0, 0, 255], 80, 48);
    let frame_two = png_checker_sized([0, 255, 0, 255], [0, 0, 0, 255], 80, 48);
    let frame_ten = png_noise_sized(80, 48);
    let expected_bytes = (frame_one.len() + frame_two.len()) as u64;

    let mut writer = RpaWriter::create(&input, 0).unwrap();
    writer.add_file("images/scene 1.png", &frame_one).unwrap();
    writer.add_file("images/scene 10.png", &frame_ten).unwrap();
    writer.add_file("images/scene 2.png", &frame_two).unwrap();
    writer.finish().unwrap();

    let options = Vp9BuildOptions {
        limit: 2,
        video_limit: 0,
        bundle_size: 2,
        min_width: 64,
        min_height: 32,
        skip_transparent: false,
        min_savings_percent: 8,
        strip_optimized: false,
        optimize_videos: false,
    };
    let cancel = AtomicBool::new(false);
    let stats = pipeline::plan_vp9(&input, &options, &[], &NoProgress, &cancel).unwrap();

    assert_eq!(stats.selected_frames, 2);
    assert_eq!(stats.selected_original_bytes, expected_bytes);
    assert_eq!(stats.planned_bundles_before_size_guard, 1);

    let _ = fs::remove_dir_all(&root);
}

#[test]
#[ignore = "requires ffmpeg on PATH"]
fn test_build_vp9_sample_writes_bundle_manifest() {
    let root = test_root();
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let input = root.join("input.rpa");
    let output = root.join("output.rpa");
    let work = root.join("work");
    let assets = root.join("android-assets");

    write_sample_rpa(&input);

    let cancel = AtomicBool::new(false);
    pipeline::build_vp9_sample(
        &input,
        &output,
        5,
        2,
        &work,
        Some(&assets),
        &[],
        &NoProgress,
        &cancel,
    )
    .unwrap();

    let mut reader = RpaReader::open(&output).unwrap();
    let index = reader.read_index().unwrap();
    assert!(index.contains_key("renpak/bundles/b000.webm"));
    assert!(index.contains_key("renpak/bundles/b001.webm"));
    assert!(index.contains_key("renpak/bundles/b002.webm"));
    assert!(index.contains_key("renpak_manifest.json"));

    let manifest_entry = index.get("renpak_manifest.json").unwrap();
    let manifest = reader.read_file_at(manifest_entry).unwrap();
    let manifest = String::from_utf8(manifest).unwrap();
    assert!(manifest.contains("\"version\": 2"));
    assert!(manifest.contains("\"mode\": \"vp9_bundle_frame\""));
    assert!(manifest.contains("\"profile\": \"source-resolution\""));
    assert!(manifest.contains("\"target\": \"renpak/bundles/b000.webm\""));
    assert!(manifest.contains("\"target\": \"renpak/bundles/b001.webm\""));
    assert!(manifest.contains("\"target\": \"renpak/bundles/b002.webm\""));
    assert!(assets.join("renpak_manifest.json").is_file());
    assert!(assets.join("renpak/bundles/b000.webm").is_file());
    assert!(assets.join("renpak/bundles/b001.webm").is_file());
    assert!(assets.join("renpak/bundles/b002.webm").is_file());

    let _ = fs::remove_dir_all(&root);
}

#[test]
#[ignore = "requires ffmpeg on PATH"]
fn test_build_vp9_filters_small_and_transparent_images() {
    let root = test_root();
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let input = root.join("input-filter.rpa");
    let output = root.join("output-filter.rpa");
    let work = root.join("work-filter");

    let mut writer = RpaWriter::create(&input, 0).unwrap();
    writer
        .add_file(
            "images/large-a.png",
            &png_bytes_sized([255, 0, 0, 255], 80, 48),
        )
        .unwrap();
    writer
        .add_file(
            "images/large-b.png",
            &png_bytes_sized([0, 255, 0, 255], 80, 48),
        )
        .unwrap();
    writer
        .add_file(
            "images/small.png",
            &png_bytes_sized([0, 0, 255, 255], 16, 16),
        )
        .unwrap();
    writer
        .add_file(
            "images/transparent.png",
            &png_bytes_sized([255, 255, 255, 128], 80, 48),
        )
        .unwrap();
    writer.finish().unwrap();

    let cancel = AtomicBool::new(false);
    let options = Vp9BuildOptions {
        limit: 10,
        video_limit: 0,
        bundle_size: 8,
        min_width: 64,
        min_height: 32,
        skip_transparent: true,
        min_savings_percent: 0,
        strip_optimized: false,
        optimize_videos: false,
    };
    pipeline::build_vp9(
        &input,
        &output,
        &options,
        &work,
        None,
        &[],
        &NoProgress,
        &cancel,
    )
    .unwrap();

    let mut reader = RpaReader::open(&output).unwrap();
    let index = reader.read_index().unwrap();
    let manifest_entry = index.get("renpak_manifest.json").unwrap();
    let manifest = reader.read_file_at(manifest_entry).unwrap();
    let manifest = String::from_utf8(manifest).unwrap();
    assert!(manifest.contains("images/large-a.png"));
    assert!(manifest.contains("images/large-b.png"));
    assert!(!manifest.contains("images/small.png"));
    assert!(!manifest.contains("images/transparent.png"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
#[ignore = "requires ffmpeg on PATH"]
fn test_build_vp9_rejects_bundles_below_savings_threshold() {
    let root = test_root();
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let input = root.join("input-guard.rpa");
    let output = root.join("output-guard.rpa");
    let work = root.join("work-guard");

    write_sample_rpa(&input);

    let cancel = AtomicBool::new(false);
    let options = Vp9BuildOptions {
        limit: 5,
        video_limit: 0,
        bundle_size: 5,
        min_width: 1,
        min_height: 1,
        skip_transparent: false,
        min_savings_percent: 100,
        strip_optimized: false,
        optimize_videos: false,
    };
    pipeline::build_vp9(
        &input,
        &output,
        &options,
        &work,
        None,
        &[],
        &NoProgress,
        &cancel,
    )
    .unwrap();

    let mut reader = RpaReader::open(&output).unwrap();
    let index = reader.read_index().unwrap();
    assert!(!index.contains_key("renpak/bundles/b000.webm"));
    assert!(index.contains_key("renpak_manifest.json"));

    let manifest_entry = index.get("renpak_manifest.json").unwrap();
    let manifest = reader.read_file_at(manifest_entry).unwrap();
    let manifest = String::from_utf8(manifest).unwrap();
    assert!(manifest.contains("\"version\": 2"));
    assert!(!manifest.contains("\"vp9_bundle_frame\""));

    let _ = fs::remove_dir_all(&root);
}

#[test]
#[ignore = "requires ffmpeg on PATH"]
fn test_build_vp9_can_strip_optimized_originals() {
    let root = test_root();
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let input = root.join("input-strip.rpa");
    let output = root.join("output-strip.rpa");
    let work = root.join("work-strip");

    write_sample_rpa(&input);

    let cancel = AtomicBool::new(false);
    let options = Vp9BuildOptions {
        limit: 5,
        video_limit: 0,
        bundle_size: 5,
        min_width: 1,
        min_height: 1,
        skip_transparent: false,
        min_savings_percent: 0,
        strip_optimized: true,
        optimize_videos: false,
    };
    pipeline::build_vp9(
        &input,
        &output,
        &options,
        &work,
        None,
        &[],
        &NoProgress,
        &cancel,
    )
    .unwrap();

    let mut reader = RpaReader::open(&output).unwrap();
    let index = reader.read_index().unwrap();
    assert!(!index.contains_key("images/a.png"));
    assert!(!index.contains_key("images/e.png"));
    assert!(index.contains_key("script.rpy"));
    assert!(index.contains_key("renpak/bundles/b000.webm"));
    assert!(index.contains_key("renpak_manifest.json"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
#[ignore = "requires ffmpeg on PATH"]
fn test_build_vp9_reencodes_mobile_videos() {
    let root = test_root();
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let input = root.join("input-video.rpa");
    let output = root.join("output-video.rpa");
    let work = root.join("work-video");
    let video_path = root.join("source-video.mp4");
    write_test_video(&video_path);

    let mut writer = RpaWriter::create(&input, 0).unwrap();
    writer
        .add_file(
            "images/large-a.png",
            &png_bytes_sized([255, 0, 0, 255], 80, 48),
        )
        .unwrap();
    writer
        .add_file(
            "images/large-b.png",
            &png_bytes_sized([0, 255, 0, 255], 80, 48),
        )
        .unwrap();
    writer
        .add_file("movies/intro.mp4", &fs::read(&video_path).unwrap())
        .unwrap();
    writer.finish().unwrap();

    let cancel = AtomicBool::new(false);
    let options = Vp9BuildOptions {
        limit: 2,
        video_limit: 1,
        bundle_size: 2,
        min_width: 1,
        min_height: 1,
        skip_transparent: false,
        min_savings_percent: 0,
        strip_optimized: true,
        optimize_videos: true,
    };
    pipeline::build_vp9(
        &input,
        &output,
        &options,
        &work,
        None,
        &[],
        &NoProgress,
        &cancel,
    )
    .unwrap();

    let mut reader = RpaReader::open(&output).unwrap();
    let index = reader.read_index().unwrap();
    assert!(index.contains_key("renpak/videos/v000.webm"));
    assert!(!index.contains_key("movies/intro.mp4"));

    let manifest_entry = index.get("renpak_manifest.json").unwrap();
    let manifest = reader.read_file_at(manifest_entry).unwrap();
    let manifest = String::from_utf8(manifest).unwrap();
    assert!(manifest.contains("\"movies/intro.mp4\""));
    assert!(manifest.contains("\"kind\": \"video\""));
    assert!(manifest.contains("\"target\": \"renpak/videos/v000.webm\""));
    assert!(manifest.contains("\"profile\": \"mobile-720\""));

    let _ = fs::remove_dir_all(&root);
}

fn write_sample_rpa(path: &Path) {
    let mut writer = RpaWriter::create(path, 0).unwrap();
    writer
        .add_file("images/a.png", &png_bytes([255, 0, 0, 255]))
        .unwrap();
    writer
        .add_file("images/b.png", &png_bytes([0, 255, 0, 255]))
        .unwrap();
    writer
        .add_file("images/c.png", &png_bytes([0, 0, 255, 255]))
        .unwrap();
    writer
        .add_file("images/d.png", &png_bytes([255, 255, 0, 255]))
        .unwrap();
    writer
        .add_file("images/e.png", &png_bytes([255, 0, 255, 255]))
        .unwrap();
    writer
        .add_file("script.rpy", b"label start:\n    return\n")
        .unwrap();
    writer.finish().unwrap();
}

fn png_bytes(pixel: [u8; 4]) -> Vec<u8> {
    png_bytes_sized(pixel, 16, 16)
}

fn png_bytes_sized(pixel: [u8; 4], width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::new();
    for _ in 0..(width * height) {
        rgba.extend_from_slice(&pixel);
    }

    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
        .unwrap();
    out
}

fn png_checker_sized(a: [u8; 4], b: [u8; 4], width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::new();
    for y in 0..height {
        for x in 0..width {
            if (x + y) % 2 == 0 {
                rgba.extend_from_slice(&a);
            } else {
                rgba.extend_from_slice(&b);
            }
        }
    }

    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
        .unwrap();
    out
}

fn png_noise_sized(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let v = ((x * 31 + y * 17 + x * y * 3) % 251) as u8;
            rgba.extend_from_slice(&[v, v.wrapping_mul(3), v.wrapping_mul(7), 255]);
        }
    }

    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
        .unwrap();
    out
}

fn write_test_video(path: &Path) {
    let status = Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg("testsrc2=duration=1:size=640x480:rate=24")
        .arg("-c:v")
        .arg("mpeg4")
        .arg("-q:v")
        .arg("2")
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

fn test_root() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!(
            "renpak-vp9-sample-test-{}-{nanos}",
            std::process::id()
        ))
}
