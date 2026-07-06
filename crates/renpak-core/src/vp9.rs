use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct BundleFrame {
    pub name: String,
    pub original_bytes: u64,
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub struct Vp9BundleOptions {
    pub gop: u32,
    pub crf: u32,
    pub cpu_used: u32,
    pub profile: String,
}

impl Default for Vp9BundleOptions {
    fn default() -> Self {
        Self {
            gop: 8,
            crf: 38,
            cpu_used: 8,
            profile: "source-resolution".to_string(),
        }
    }
}

pub struct EncodedBundle {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub frame_count: u32,
}

pub struct Vp9VideoOptions {
    pub crf: u32,
    pub cpu_used: u32,
    pub max_height: u32,
    pub profile: String,
}

impl Default for Vp9VideoOptions {
    fn default() -> Self {
        Self {
            crf: 38,
            cpu_used: 8,
            max_height: 720,
            profile: "mobile-720".to_string(),
        }
    }
}

pub struct EncodedVideo {
    pub data: Vec<u8>,
}

pub fn encode_vp9_bundle(
    frames: &[BundleFrame],
    work_root: &Path,
    options: &Vp9BundleOptions,
) -> Result<EncodedBundle, String> {
    if frames.is_empty() {
        return Err("vp9 bundle requires at least one frame".to_string());
    }

    let width = frames[0].width;
    let height = frames[0].height;
    if width == 0 || height == 0 {
        return Err("vp9 bundle frame dimensions must be non-zero".to_string());
    }

    for frame in frames {
        if frame.width != width || frame.height != height {
            return Err(format!(
                "vp9 bundle frame dimensions differ: {} is {}x{}, expected {}x{}",
                frame.name, frame.width, frame.height, width, height
            ));
        }
    }

    fs::create_dir_all(work_root).map_err(|e| format!("create vp9 work root: {e}"))?;
    let scratch = unique_scratch_dir(work_root);
    fs::create_dir_all(&scratch).map_err(|e| format!("create vp9 scratch: {e}"))?;

    let result = encode_vp9_bundle_inner(frames, &scratch, options, width, height);
    let cleanup_result = fs::remove_dir_all(&scratch);

    match (result, cleanup_result) {
        (Ok(bundle), _) => Ok(bundle),
        (Err(err), Ok(())) => Err(err),
        (Err(err), Err(cleanup)) => Err(format!("{err}; cleanup failed: {cleanup}")),
    }
}

fn encode_vp9_bundle_inner(
    frames: &[BundleFrame],
    scratch: &Path,
    options: &Vp9BundleOptions,
    width: u32,
    height: u32,
) -> Result<EncodedBundle, String> {
    for (i, frame) in frames.iter().enumerate() {
        let path = scratch.join(format!("frame_{i:06}.png"));
        image::save_buffer_with_format(
            &path,
            &frame.rgba,
            frame.width,
            frame.height,
            image::ColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .map_err(|e| format!("write vp9 frame {}: {e}", frame.name))?;
    }

    let output_path = scratch.join("bundle.webm");
    let input_pattern = scratch.join("frame_%06d.png");
    let output = Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-framerate")
        .arg("1")
        .arg("-i")
        .arg(&input_pattern)
        .arg("-c:v")
        .arg("libvpx-vp9")
        .arg("-deadline")
        .arg("realtime")
        .arg("-cpu-used")
        .arg(options.cpu_used.to_string())
        .arg("-row-mt")
        .arg("1")
        .arg("-crf")
        .arg(options.crf.to_string())
        .arg("-b:v")
        .arg("0")
        .arg("-g")
        .arg(options.gop.to_string())
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg(&output_path)
        .output()
        .map_err(|e| format!("run ffmpeg: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "ffmpeg vp9 bundle failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let data = fs::read(&output_path).map_err(|e| format!("read vp9 bundle: {e}"))?;
    Ok(EncodedBundle {
        data,
        width,
        height,
        frame_count: frames.len() as u32,
    })
}

fn unique_scratch_dir(work_root: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    work_root.join(format!("vp9-{nanos}-{}", std::process::id()))
}

pub fn encode_vp9_video(
    input_name: &str,
    input_data: &[u8],
    work_root: &Path,
    options: &Vp9VideoOptions,
) -> Result<EncodedVideo, String> {
    fs::create_dir_all(work_root).map_err(|e| format!("create vp9 work root: {e}"))?;
    let scratch = unique_scratch_dir(work_root);
    fs::create_dir_all(&scratch).map_err(|e| format!("create vp9 scratch: {e}"))?;

    let result = encode_vp9_video_inner(input_name, input_data, &scratch, options);
    let cleanup_result = fs::remove_dir_all(&scratch);

    match (result, cleanup_result) {
        (Ok(video), _) => Ok(video),
        (Err(err), Ok(())) => Err(err),
        (Err(err), Err(cleanup)) => Err(format!("{err}; cleanup failed: {cleanup}")),
    }
}

fn encode_vp9_video_inner(
    input_name: &str,
    input_data: &[u8],
    scratch: &Path,
    options: &Vp9VideoOptions,
) -> Result<EncodedVideo, String> {
    let suffix = Path::new(input_name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("webm");
    let input_path = scratch.join(format!("input.{suffix}"));
    let output_path = scratch.join("video.webm");
    fs::write(&input_path, input_data).map_err(|e| format!("write vp9 video input: {e}"))?;

    let scale = format!("scale=-2:min({}\\,ih):flags=lanczos", options.max_height);
    let output = Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-i")
        .arg(&input_path)
        .arg("-vf")
        .arg(scale)
        .arg("-c:v")
        .arg("libvpx-vp9")
        .arg("-deadline")
        .arg("realtime")
        .arg("-cpu-used")
        .arg(options.cpu_used.to_string())
        .arg("-row-mt")
        .arg("1")
        .arg("-crf")
        .arg(options.crf.to_string())
        .arg("-b:v")
        .arg("0")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-c:a")
        .arg("copy")
        .arg(&output_path)
        .output()
        .map_err(|e| format!("run ffmpeg: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "ffmpeg vp9 video failed for {input_name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let data = fs::read(&output_path).map_err(|e| format!("read vp9 video: {e}"))?;
    Ok(EncodedVideo { data })
}

#[cfg(test)]
mod tests {
    use super::{Vp9BundleOptions, Vp9VideoOptions};

    #[test]
    fn default_profile_matches_current_resolution_behavior() {
        assert_eq!(Vp9BundleOptions::default().profile, "source-resolution");
    }

    #[test]
    fn default_video_profile_matches_mobile_baseline() {
        let options = Vp9VideoOptions::default();
        assert_eq!(options.profile, "mobile-720");
        assert_eq!(options.max_height, 720);
        assert_eq!(options.crf, 38);
    }
}
