//! Dora 图像查看节点：Arrow `forge_msgs.Image` / `CompressedImage` + legacy bytes，eframe/egui 显示。

mod config;

use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use arrow_array::{Array, BinaryArray, LargeBinaryArray, RecordBatch, StructArray};
use config::{RendererBackend, ResolvedSettings, load_config_file, resolve_settings};

use dora_node_api::{DoraNode, Event, EventStream};
use eframe::egui;
use forge_msgs::image::ImageError;
use forge_msgs::{CompressedImage, Image};

#[derive(Debug)]
struct ImageFrame {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

/// 跨线程的最新帧 mailbox。每路 input 最多保留一帧，避免 UI 落后时无界积压。
#[derive(Debug, Default)]
struct FrameMailbox {
    pending: Mutex<HashMap<String, ImageFrame>>,
}

impl FrameMailbox {
    fn publish(&self, id: String, frame: ImageFrame) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, frame);
    }

    fn take_all(&self) -> HashMap<String, ImageFrame> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *pending)
    }
}

fn arrow_to_record_batch(data: &dora_node_api::ArrowData) -> Option<RecordBatch> {
    let arr = data.deref();
    let sa = arr.as_any().downcast_ref::<StructArray>()?;
    Some(RecordBatch::from(sa.clone()))
}

fn try_decode_forge_image(batch: &RecordBatch) -> Option<(u32, u32, Vec<u8>)> {
    if let Ok(img) = Image::from_record_batch(batch) {
        if img.width == 0 || img.height == 0 {
            return None;
        }
        let rgb = raw_image_to_rgb(&img).ok()?;
        return Some((img.width, img.height, rgb));
    }

    let img = CompressedImage::from_record_batch(batch).ok()?;
    let arr = img.to_rgb8_ndarray().ok()?;
    let (h, w, _) = arr.dim();
    if h == 0 || w == 0 {
        return None;
    }
    Some((w as u32, h as u32, arr.into_raw_vec_and_offset().0))
}

fn raw_image_to_rgb(img: &Image) -> Result<Vec<u8>, ImageError> {
    let width = img.width as usize;
    let height = img.height as usize;
    let step = img.step as usize;
    let data = img.data.as_ref();
    let mut rgb = Vec::with_capacity(width * height * 3);

    match img.encoding.as_str() {
        "rgb8" => {
            for y in 0..height {
                let row = &data[y * step..y * step + width * 3];
                rgb.extend_from_slice(row);
            }
            Ok(rgb)
        }
        "bgr8" => {
            for y in 0..height {
                let row = &data[y * step..y * step + width * 3];
                for pixel in row.chunks_exact(3) {
                    rgb.extend([pixel[2], pixel[1], pixel[0]]);
                }
            }
            Ok(rgb)
        }
        "mono8" => {
            for y in 0..height {
                let row = &data[y * step..y * step + width];
                for &g in row {
                    rgb.extend([g, g, g]);
                }
            }
            Ok(rgb)
        }
        "16UC1" => {
            let values = read_u16_image(data, width, height, step)?;
            grayscale_u16_to_rgb(&values)
        }
        "32FC1" => {
            let values = read_f32_image(data, width, height, step)?;
            grayscale_f32_to_rgb(&values)
        }
        other => Err(ImageError::UnsupportedEncoding(other.to_string())),
    }
}

fn read_u16_image(
    data: &[u8],
    width: usize,
    height: usize,
    step: usize,
) -> Result<Vec<u16>, ImageError> {
    let mut values = Vec::with_capacity(width * height);
    for y in 0..height {
        let row = &data[y * step..y * step + width * 2];
        for chunk in row.chunks_exact(2) {
            values.push(u16::from_le_bytes([chunk[0], chunk[1]]));
        }
    }
    Ok(values)
}

fn read_f32_image(
    data: &[u8],
    width: usize,
    height: usize,
    step: usize,
) -> Result<Vec<f32>, ImageError> {
    let mut values = Vec::with_capacity(width * height);
    for y in 0..height {
        let row = &data[y * step..y * step + width * 4];
        for chunk in row.chunks_exact(4) {
            values.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
    }
    Ok(values)
}

fn grayscale_u16_to_rgb(values: &[u16]) -> Result<Vec<u8>, ImageError> {
    let max = values.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return Ok(vec![0; values.len() * 3]);
    }
    let mut rgb = Vec::with_capacity(values.len() * 3);
    for &value in values {
        let g = ((value as f32 / max as f32) * 255.0).round() as u8;
        rgb.extend([g, g, g]);
    }
    Ok(rgb)
}

fn grayscale_f32_to_rgb(values: &[f32]) -> Result<Vec<u8>, ImageError> {
    let finite: Vec<f32> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if finite.is_empty() {
        return Ok(vec![0; values.len() * 3]);
    }
    let min = finite.iter().copied().fold(f32::INFINITY, f32::min);
    let max = finite.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if max <= min {
        return Ok(vec![0; values.len() * 3]);
    }
    let mut rgb = Vec::with_capacity(values.len() * 3);
    for &value in values {
        let g = if value.is_finite() {
            (((value - min) / (max - min)) * 255.0).clamp(0.0, 255.0) as u8
        } else {
            0
        };
        rgb.extend([g, g, g]);
    }
    Ok(rgb)
}

fn try_decode_legacy_bytes(
    data: &dora_node_api::ArrowData,
    settings: &ResolvedSettings,
) -> Option<(u32, u32, Vec<u8>)> {
    let w = settings.legacy_width;
    let h = settings.legacy_height;
    let c = settings.legacy_channels;
    if w == 0 || h == 0 || c == 0 || c > 4 {
        return None;
    }

    let arr = data.deref();
    let bytes: &[u8] = if let Some(lb) = arr.as_any().downcast_ref::<LargeBinaryArray>() {
        if lb.is_empty() || lb.is_null(0) {
            return None;
        }
        lb.value(0)
    } else {
        let b = arr.as_any().downcast_ref::<BinaryArray>()?;
        if b.is_empty() || b.is_null(0) {
            return None;
        }
        b.value(0)
    };

    let expected = (w * h * c) as usize;
    if bytes.len() != expected {
        return None;
    }

    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    match c {
        1 => {
            for &g in bytes {
                rgb.extend([g, g, g]);
            }
        }
        3 => {
            if settings.legacy_bgr {
                for chunk in bytes.chunks_exact(3) {
                    rgb.extend([chunk[2], chunk[1], chunk[0]]);
                }
            } else {
                rgb.extend_from_slice(bytes);
            }
        }
        _ => return None,
    }
    Some((w, h, rgb))
}

fn decode_input(
    data: &dora_node_api::ArrowData,
    settings: &ResolvedSettings,
) -> Option<(u32, u32, Vec<u8>)> {
    if let Some(batch) = arrow_to_record_batch(data)
        && let Some(t) = try_decode_forge_image(&batch)
    {
        return Some(t);
    }
    try_decode_legacy_bytes(data, settings)
}

fn input_allowed(id: &str, filter: &Option<Vec<String>>) -> bool {
    if id == "tick" {
        return false;
    }
    match filter {
        None => true,
        Some(ids) => ids.iter().any(|x| x == id),
    }
}

fn run_dora_thread(
    mut events: EventStream,
    frames: Arc<FrameMailbox>,
    settings: ResolvedSettings,
    stop: Arc<AtomicBool>,
    decode_warned: Arc<AtomicBool>,
    egui_ctx: egui::Context,
) {
    let wake = || egui_ctx.request_repaint_of(egui::ViewportId::ROOT);

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let event = match events.recv() {
            Some(e) => e,
            None => break,
        };

        match event {
            Event::Input { id, data, .. } => {
                let id_str = id.as_str().to_string();
                if !input_allowed(&id_str, &settings.filter_ids) {
                    continue;
                }
                match decode_input(&data, &settings) {
                    Some((width, height, rgb)) => {
                        frames.publish(id_str, ImageFrame { width, height, rgb });
                        wake();
                    }
                    None => {
                        if !decode_warned.swap(true, Ordering::Relaxed) {
                            eprintln!(
                                "[image_viewer] 无法解码输入（forge_msgs.Image / CompressedImage Arrow 或 legacy raw bytes）。"
                            );
                        }
                    }
                }
            }
            Event::Stop(_) => {
                stop.store(true, Ordering::Relaxed);
                wake();
                break;
            }
            Event::Error(msg) => {
                eprintln!("[image_viewer] error: {msg}");
                stop.store(true, Ordering::Relaxed);
                wake();
                break;
            }
            _ => {}
        }
    }
    stop.store(true, Ordering::Relaxed);
    wake();
}

fn install_shutdown_signal_handler(stop: Arc<AtomicBool>, egui_ctx: egui::Context) {
    if let Err(err) = ctrlc::set_handler(move || {
        if !stop.swap(true, Ordering::Relaxed) {
            egui_ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
    }) {
        eprintln!("[image_viewer] failed to install shutdown signal handler: {err}");
    }
}

struct ImageViewerApp {
    frames: Arc<FrameMailbox>,
    textures: HashMap<String, egui::TextureHandle>,
    stop: Arc<AtomicBool>,
    /// 用户手动关闭的 input 窗口；本次运行中忽略该 input 的后续帧。
    closed_input_ids: HashSet<String>,
    /// 已应用到主 viewport 的 input 与纹理尺寸，避免每帧重复发送窗口命令。
    main_viewport: Option<(String, [usize; 2])>,
    /// Deferred viewport 通过这里把关闭事件回传给主 App。
    close_requests: Arc<Mutex<HashSet<String>>>,
}

impl ImageViewerApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        frames: Arc<FrameMailbox>,
        stop: Arc<AtomicBool>,
    ) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        Self {
            frames,
            textures: HashMap::new(),
            stop,
            closed_input_ids: HashSet::new(),
            main_viewport: None,
            close_requests: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    fn apply_pending_frames(&mut self, ctx: &egui::Context) -> HashSet<String> {
        let mut updated_ids = HashSet::new();
        for (id, frame) in self.frames.take_all() {
            if self.closed_input_ids.contains(&id) {
                continue;
            }

            let size = [frame.width as usize, frame.height as usize];
            let Some(expected_len) = size[0]
                .checked_mul(size[1])
                .and_then(|pixels| pixels.checked_mul(3))
            else {
                continue;
            };
            if size.contains(&0) || frame.rgb.len() != expected_len {
                continue;
            }

            let color_image = egui::ColorImage::from_rgb(size, &frame.rgb);
            match self.textures.get_mut(&id) {
                Some(texture) => texture.set(color_image, egui::TextureOptions::LINEAR),
                None => {
                    let texture = ctx.load_texture(
                        format!("img_{id}"),
                        color_image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.textures.insert(id.clone(), texture);
                }
            }
            updated_ids.insert(id);
        }
        updated_ids
    }

    fn initial_image_window_size(texture_size: egui::Vec2) -> egui::Vec2 {
        texture_size.max(egui::vec2(1.0, 1.0))
    }

    fn sorted_texture_ids(&self) -> Vec<String> {
        let mut ids: Vec<_> = self.textures.keys().cloned().collect();
        ids.sort();
        ids
    }

    fn image_viewport_id(id: &str) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("image_viewer", id))
    }

    fn hide_input_window(&mut self, id: &str) {
        self.textures.remove(id);
        self.closed_input_ids.insert(id.to_owned());
    }

    fn apply_close_requests(&mut self) {
        let requests = {
            let mut requests = self
                .close_requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut *requests)
        };
        for id in requests {
            self.hide_input_window(&id);
        }
    }

    fn handle_main_viewport_close(&mut self, ctx: &egui::Context, main_id: Option<&str>) {
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }

        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if let Some(id) = main_id {
            self.hide_input_window(id);
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
    }

    fn show_main_viewport(&mut self, ctx: &egui::Context, main_id: Option<&str>) {
        let Some(id) = main_id else {
            if self.main_viewport.take().is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
            return;
        };
        let Some(tex) = self.textures.get(id) else {
            return;
        };

        let texture_size = tex.size();
        let viewport_state = (id.to_owned(), texture_size);
        if self.main_viewport.as_ref() != Some(&viewport_state) {
            let title = format!("image_viewer:{id}");
            let image_size = Self::initial_image_window_size(tex.size_vec2());
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(image_size));
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(image_size));
            ctx.send_viewport_cmd(egui::ViewportCommand::MaxInnerSize(image_size));
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            self.main_viewport = Some(viewport_state);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                ui.image((tex.id(), tex.size_vec2()));
            });
    }

    fn show_image_viewports(&self, ctx: &egui::Context, main_id: Option<&str>) {
        for id in self.sorted_texture_ids() {
            if main_id == Some(id.as_str()) {
                continue;
            }
            let Some(texture) = self.textures.get(&id).cloned() else {
                continue;
            };

            let title = format!("image_viewer:{id}");
            let viewport_id = Self::image_viewport_id(&id);
            let image_size = Self::initial_image_window_size(texture.size_vec2());
            let builder = egui::ViewportBuilder::default()
                .with_title(title.clone())
                .with_inner_size(image_size)
                .with_min_inner_size(image_size)
                .with_max_inner_size(image_size)
                .with_resizable(false)
                .with_maximize_button(false);
            let close_requests = Arc::clone(&self.close_requests);

            ctx.show_viewport_deferred(viewport_id, builder, move |ctx, class| {
                let mut close_requested = ctx.input(|i| i.viewport().close_requested());
                if matches!(class, egui::ViewportClass::Embedded) {
                    let mut open = true;
                    egui::Window::new(title.as_str())
                        .open(&mut open)
                        .resizable(false)
                        .show(ctx, |ui| {
                            ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                            ui.image((texture.id(), texture.size_vec2()));
                        });
                    close_requested |= !open;
                } else {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::none())
                        .show(ctx, |ui| {
                            ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                            ui.image((texture.id(), texture.size_vec2()));
                        });
                }

                if close_requested {
                    close_requests
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(id.clone());
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
            });
        }
    }
}

impl eframe::App for ImageViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_close_requests();
        let updated_ids = self.apply_pending_frames(ctx);
        if self.stop.load(Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let main_id = self.sorted_texture_ids().into_iter().next();
        self.handle_main_viewport_close(ctx, main_id.as_deref());
        let main_id = self.sorted_texture_ids().into_iter().next();
        self.show_main_viewport(ctx, main_id.as_deref());
        self.show_image_viewports(ctx, main_id.as_deref());

        // Deferred 子窗口独立重绘。先声明 viewport，再只唤醒本轮收到新纹理的窗口；
        // WGPU 会在父 viewport 返回后提交纹理增量，随后子窗口再安全地引用该纹理。
        for id in updated_ids {
            if main_id.as_deref() != Some(id.as_str()) {
                ctx.request_repaint_of(Self::image_viewport_id(&id));
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct CliArgs {
    config: Option<String>,
    input_id: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    channels: Option<u32>,
    bgr: bool,
    renderer: Option<RendererBackend>,
}

fn parse_cli() -> eyre::Result<CliArgs> {
    let mut args = CliArgs::default();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => args.config = it.next(),
            "--input-id" => args.input_id = it.next(),
            "--width" => {
                if let Some(v) = it.next()
                    && let Ok(n) = v.parse()
                {
                    args.width = Some(n);
                }
            }
            "--height" => {
                if let Some(v) = it.next()
                    && let Ok(n) = v.parse()
                {
                    args.height = Some(n);
                }
            }
            "--channels" => {
                if let Some(v) = it.next()
                    && let Ok(n) = v.parse()
                {
                    args.channels = Some(n);
                }
            }
            "--bgr" => args.bgr = true,
            "--renderer" => {
                let value = it
                    .next()
                    .ok_or_else(|| eyre::eyre!("--renderer requires `wgpu` or `glow`"))?;
                args.renderer = Some(match value.as_str() {
                    "wgpu" => RendererBackend::Wgpu,
                    "glow" => RendererBackend::Glow,
                    _ => {
                        return Err(eyre::eyre!(
                            "unsupported renderer `{value}`; expected `wgpu` or `glow`"
                        ));
                    }
                });
            }
            _ => {}
        }
    }
    Ok(args)
}

fn main() -> eyre::Result<()> {
    let cli = parse_cli()?;
    let file_cfg = load_config_file(cli.config.as_deref())?;
    let settings = resolve_settings(
        file_cfg,
        cli.input_id,
        cli.width,
        cli.height,
        cli.channels,
        cli.bgr,
        cli.renderer,
    );
    let renderer = match settings.renderer {
        RendererBackend::Wgpu => eframe::Renderer::Wgpu,
        RendererBackend::Glow => eframe::Renderer::Glow,
    };

    let (node, events) = DoraNode::init_from_env()?;

    let frames = Arc::new(FrameMailbox::default());
    let stop = Arc::new(AtomicBool::new(false));
    let decode_warned = Arc::new(AtomicBool::new(false));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("forge image_viewer")
            .with_inner_size([1.0, 1.0])
            .with_visible(false),
        renderer,
        ..Default::default()
    };

    let stop_ui = Arc::clone(&stop);
    eframe::run_native(
        "image_viewer",
        options,
        Box::new(move |cc| {
            let egui_ctx = cc.egui_ctx.clone();
            install_shutdown_signal_handler(Arc::clone(&stop_ui), egui_ctx.clone());
            let stop_bg = Arc::clone(&stop_ui);
            let settings_clone = settings.clone();
            let frames_bg = Arc::clone(&frames);
            let warned = Arc::clone(&decode_warned);
            std::thread::spawn(move || {
                let _keep_node = node;
                run_dora_thread(events, frames_bg, settings_clone, stop_bg, warned, egui_ctx);
            });
            Ok(Box::new(ImageViewerApp::new(cc, frames, stop_ui)))
        }),
    )
    .map_err(|e| eyre::eyre!("eframe: {e}"))?;

    stop.store(true, Ordering::Relaxed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(value: u8) -> ImageFrame {
        ImageFrame {
            width: 1,
            height: 1,
            rgb: vec![value; 3],
        }
    }

    #[test]
    fn frame_mailbox_keeps_only_the_latest_frame_per_input() {
        let mailbox = FrameMailbox::default();
        mailbox.publish("left".to_owned(), frame(1));
        mailbox.publish("right".to_owned(), frame(2));
        mailbox.publish("left".to_owned(), frame(3));

        let pending = mailbox.take_all();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending["left"].rgb, vec![3; 3]);
        assert_eq!(pending["right"].rgb, vec![2; 3]);
        assert!(mailbox.take_all().is_empty());
    }
}
