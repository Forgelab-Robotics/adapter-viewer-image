//! YAML / CLI 配置，对齐 Python `config.py` 与 `main.py` 参数。

use std::env;
use std::fs;
use std::path::Path;

use eyre::WrapErr;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RendererBackend {
    #[default]
    Wgpu,
    Glow,
}

#[derive(Debug, Clone, Deserialize)]
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
        let raw = fs::read_to_string(path)
            .wrap_err_with(|| format!("failed to read config {}", path.display()))?;
        serde_yaml::from_str(&raw)
            .wrap_err_with(|| format!("failed to parse config {}", path.display()))
    }
}

pub fn load_config_file(config_path: Option<&str>) -> eyre::Result<ImageViewerConfig> {
    let path = config_path
        .map(|s| s.to_string())
        .or_else(|| env::var("IMAGE_VIEWER_CONFIG").ok());
    match path {
        Some(p) if !p.is_empty() => ImageViewerConfig::from_yaml_path(Path::new(&p)),
        _ => Ok(ImageViewerConfig::default()),
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
) -> ResolvedSettings {
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

    ResolvedSettings {
        filter_ids,
        legacy_width,
        legacy_height,
        legacy_channels,
        legacy_bgr,
        renderer,
    }
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
        let settings = resolve_settings(config, None, None, None, None, false, None);
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
        );
        assert_eq!(settings.renderer, RendererBackend::Wgpu);
    }

    #[test]
    fn explicit_default_legacy_dimensions_override_yaml() {
        let config: ImageViewerConfig =
            serde_yaml::from_str("width: 1280\nheight: 720\nchannels: 1").unwrap();
        let settings = resolve_settings(config, None, Some(640), Some(480), Some(3), false, None);
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
}
