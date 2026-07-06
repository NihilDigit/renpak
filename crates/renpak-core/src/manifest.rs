use std::collections::BTreeMap;

use serde::Serialize;

pub const MANIFEST_PATH: &str = "renpak_manifest.json";
pub const MANIFEST_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize)]
pub struct Manifest {
    pub version: u32,
    pub assets: BTreeMap<String, ManifestAsset>,
}

impl Manifest {
    pub fn new() -> Self {
        Self {
            version: MANIFEST_VERSION,
            assets: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, original: String, asset: ManifestAsset) {
        self.assets.insert(original, asset);
    }

    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        Ok(json)
    }
}

impl Default for Manifest {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ManifestAsset {
    pub kind: AssetKind,
    pub mode: AssetMode,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<Codec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gop: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

impl ManifestAsset {
    pub fn avif(target: String, width: Option<u32>, height: Option<u32>, profile: String) -> Self {
        Self {
            kind: AssetKind::Image,
            mode: AssetMode::Avif,
            target,
            width,
            height,
            codec: Some(Codec::Avif),
            frame: None,
            gop: None,
            profile: Some(profile),
        }
    }

    pub fn vp9_bundle_frame(
        target: String,
        frame: u32,
        width: u32,
        height: u32,
        gop: u32,
        profile: String,
    ) -> Self {
        Self {
            kind: AssetKind::Image,
            mode: AssetMode::Vp9BundleFrame,
            target,
            width: Some(width),
            height: Some(height),
            codec: Some(Codec::Vp9),
            frame: Some(frame),
            gop: Some(gop),
            profile: Some(profile),
        }
    }

    pub fn video_vp9(
        target: String,
        width: Option<u32>,
        height: Option<u32>,
        profile: String,
    ) -> Self {
        Self {
            kind: AssetKind::Video,
            mode: AssetMode::File,
            target,
            width,
            height,
            codec: Some(Codec::Vp9),
            frame: None,
            gop: None,
            profile: Some(profile),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Image,
    Video,
    Audio,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetMode {
    Avif,
    File,
    Passthrough,
    Vp9BundleFrame,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    Avif,
    Vp9,
    Opus,
    Copy,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_v2_manifest_shape() {
        let mut manifest = Manifest::new();
        manifest.insert(
            "images/foo.jpg".to_string(),
            ManifestAsset::avif(
                "images/foo.avif".to_string(),
                Some(1920),
                Some(1080),
                "legacy-avif".to_string(),
            ),
        );

        let json = manifest.to_json_pretty().unwrap();
        assert!(json.contains("\"version\": 2"));
        assert!(json.contains("\"assets\""));
        assert!(json.contains("\"mode\": \"avif\""));
        assert!(json.contains("\"codec\": \"avif\""));
    }
}
