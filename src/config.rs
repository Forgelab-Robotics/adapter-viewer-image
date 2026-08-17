//! Strict YAML and CLI configuration resolution.

use std::env;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use clap::ValueEnum;
use eyre::{WrapErr, bail};
use serde::Deserialize;

pub const MAX_IMAGE_DIMENSION: u32 = 8_192;
pub const MAX_IMAGE_PIXELS: usize = 32 * 1024 * 1024;
pub const MAX_INPUTS: usize = 16;
pub const MAX_INPUT_ID_LEN: usize = 256;
const MAX_CONFIG_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum RendererBackend {
    #[default]
    Wgpu,
    Glow,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageViewerConfig {
    #[serde(default)]
    pub images: Vec<String>,
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default = "default_channels")]
    pub channels: u32,
    #[serde(default)]
    pub bgr: bool,
    #[serde(default)]
    pub renderer: RendererBackend,
}

fn default_width() -> u32 {
    640
}

fn default_height() -> u32 {
    480
}

fn default_channels() -> u32 {
    3
}

impl Default for ImageViewerConfig {
    fn default() -> Self {
        Self {
            images: Vec::new(),
            width: default_width(),
            height: default_height(),
            channels: default_channels(),
            bgr: false,
            renderer: RendererBackend::default(),
        }
    }
}

impl ImageViewerConfig {
    pub fn from_yaml_path(path: &Path) -> eyre::Result<Self> {
        let file = File::open(path)
            .wrap_err_with(|| format!("failed to read config {}", path.display()))?;
        let mut raw = String::new();
        file.take((MAX_CONFIG_BYTES + 1) as u64)
            .read_to_string(&mut raw)
            .wrap_err_with(|| format!("failed to read config {}", path.display()))?;
        if raw.len() > MAX_CONFIG_BYTES {
            bail!(
                "config {} exceeds the {MAX_CONFIG_BYTES}-byte limit",
                path.display()
            );
        }
        let config: Self = serde_yaml::from_str(&raw)
            .wrap_err_with(|| format!("failed to parse config {}", path.display()))?;
        config
            .validate()
            .wrap_err_with(|| format!("invalid config {}", path.display()))?;
        Ok(config)
    }

    fn validate(&self) -> eyre::Result<()> {
        validate_legacy_dimensions(self.width, self.height, self.channels)?;
        validate_input_ids(&self.images)
    }
}

pub fn validate_image_dimensions(width: u32, height: u32) -> eyre::Result<usize> {
    if width == 0 || height == 0 {
        bail!("image dimensions must be non-zero");
    }
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        bail!(
            "image dimensions {width}x{height} exceed the maximum {MAX_IMAGE_DIMENSION}x{MAX_IMAGE_DIMENSION}"
        );
    }
    let pixels = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or_else(|| eyre::eyre!("image dimensions {width}x{height} overflow this platform"))?;
    if pixels > MAX_IMAGE_PIXELS {
        bail!("image contains {pixels} pixels, exceeding the {MAX_IMAGE_PIXELS} pixel limit");
    }
    Ok(pixels)
}

fn validate_legacy_dimensions(width: u32, height: u32, channels: u32) -> eyre::Result<()> {
    validate_image_dimensions(width, height)?;
    if !matches!(channels, 1 | 3) {
        bail!("legacy channels must be 1 or 3, got {channels}");
    }
    Ok(())
}

fn validate_input_ids(ids: &[String]) -> eyre::Result<()> {
    if ids.len() > MAX_INPUTS {
        bail!("at most {MAX_INPUTS} image inputs may be configured");
    }
    let mut unique = std::collections::HashSet::new();
    for id in ids {
        if id.trim().is_empty() {
            bail!("image input IDs must not be empty");
        }
        if id == "tick" {
            bail!("`tick` is reserved and cannot be used as an image input ID");
        }
        if id.len() > MAX_INPUT_ID_LEN {
            bail!("image input ID exceeds the {MAX_INPUT_ID_LEN}-byte limit");
        }
        if !unique.insert(id) {
            bail!("duplicate image input ID `{id}`");
        }
    }
    Ok(())
}

pub fn load_config_file(config_path: Option<&str>) -> eyre::Result<ImageViewerConfig> {
    let path = config_path
        .map(PathBuf::from)
        .or_else(|| env::var_os("IMAGE_VIEWER_CONFIG").map(PathBuf::from));
    match path {
        Some(path) if path.as_os_str().is_empty() => bail!("config path must not be empty"),
        Some(path) => ImageViewerConfig::from_yaml_path(&path),
        None => Ok(ImageViewerConfig::default()),
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedSettings {
    pub filter_ids: Option<Vec<String>>,
    pub legacy_width: u32,
    pub legacy_height: u32,
    pub legacy_channels: u32,
    pub legacy_bgr: bool,
    pub renderer: RendererBackend,
}

/// 显式 CLI 参数覆盖 YAML；未提供时使用 YAML 值或内置默认值。
pub fn resolve_settings(
    config: ImageViewerConfig,
    input_id: Option<String>,
    cli_width: Option<u32>,
    cli_height: Option<u32>,
    cli_channels: Option<u32>,
    cli_bgr: bool,
    cli_renderer: Option<RendererBackend>,
) -> eyre::Result<ResolvedSettings> {
    let filter_ids = if let Some(id) = input_id {
        Some(vec![id])
    } else if !config.images.is_empty() {
        Some(config.images.clone())
    } else {
        None
    };

    let legacy_width = cli_width.unwrap_or(config.width);
    let legacy_height = cli_height.unwrap_or(config.height);
    let legacy_channels = cli_channels.unwrap_or(config.channels);
    let legacy_bgr = cli_bgr || config.bgr;
    let renderer = cli_renderer.unwrap_or(config.renderer);

    validate_legacy_dimensions(legacy_width, legacy_height, legacy_channels)?;
    if let Some(ids) = filter_ids.as_deref() {
        validate_input_ids(ids)?;
    }

    Ok(ResolvedSettings {
        filter_ids,
        legacy_width,
        legacy_height,
        legacy_channels,
        legacy_bgr,
        renderer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_defaults_to_wgpu() {
        let config: ImageViewerConfig = serde_yaml::from_str("{}").unwrap();
        assert_eq!(config.renderer, RendererBackend::Wgpu);
    }

    #[test]
    fn yaml_renderer_is_used_without_cli_override() {
        let config: ImageViewerConfig = serde_yaml::from_str("renderer: glow").unwrap();
        let settings = resolve_settings(config, None, None, None, None, false, None).unwrap();
        assert_eq!(settings.renderer, RendererBackend::Glow);
    }

    #[test]
    fn cli_renderer_overrides_yaml() {
        let config: ImageViewerConfig = serde_yaml::from_str("renderer: glow").unwrap();
        let settings = resolve_settings(
            config,
            None,
            None,
            None,
            None,
            false,
            Some(RendererBackend::Wgpu),
        )
        .unwrap();
        assert_eq!(settings.renderer, RendererBackend::Wgpu);
    }

    #[test]
    fn explicit_default_legacy_dimensions_override_yaml() {
        let config: ImageViewerConfig =
            serde_yaml::from_str("width: 1280\nheight: 720\nchannels: 1").unwrap();
        let settings =
            resolve_settings(config, None, Some(640), Some(480), Some(3), false, None).unwrap();
        assert_eq!(settings.legacy_width, 640);
        assert_eq!(settings.legacy_height, 480);
        assert_eq!(settings.legacy_channels, 3);
    }

    #[test]
    fn explicit_missing_config_is_an_error() {
        let error = load_config_file(Some("/__forge_image_viewer_missing__/viewer.example.yaml"))
            .unwrap_err();
        assert!(error.to_string().contains("failed to read config"));
    }

    #[test]
    fn unknown_yaml_fields_are_rejected() {
        let error = serde_yaml::from_str::<ImageViewerConfig>("widht: 640").unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn invalid_legacy_channels_are_rejected() {
        let error = resolve_settings(
            ImageViewerConfig::default(),
            None,
            None,
            None,
            Some(4),
            false,
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("channels must be 1 or 3"));
    }

    #[test]
    fn oversized_images_are_rejected_without_overflow() {
        let error = validate_image_dimensions(u32::MAX, u32::MAX).unwrap_err();
        assert!(error.to_string().contains("exceed the maximum"));
    }

    #[test]
    fn duplicate_input_ids_are_rejected() {
        let config: ImageViewerConfig = serde_yaml::from_str("images: [camera, camera]").unwrap();
        let error = resolve_settings(config, None, None, None, None, false, None).unwrap_err();
        assert!(error.to_string().contains("duplicate image input ID"));
    }

    #[test]
    fn reserved_tick_input_is_rejected() {
        let config: ImageViewerConfig = serde_yaml::from_str("images: [tick]").unwrap();
        let error = resolve_settings(config, None, None, None, None, false, None).unwrap_err();
        assert!(error.to_string().contains("reserved"));
    }

    #[test]
    fn explicit_empty_config_path_is_rejected() {
        let error = load_config_file(Some("")).unwrap_err();
        assert!(error.to_string().contains("must not be empty"));
    }
}
