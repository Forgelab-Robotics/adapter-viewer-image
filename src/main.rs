//! Dora 图像查看节点：Arrow `forge_msgs.Image` / `CompressedImage` + legacy bytes，eframe/egui 显示。

mod config;

use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arrow_array::{Array, BinaryArray, LargeBinaryArray, RecordBatch, StructArray};
use config::{RendererBackend, ResolvedSettings, load_config_file, resolve_settings};
use crossbeam_channel::{Receiver, Sender, unbounded};
use dora_node_api::{DoraNode, Event, EventStream};
use eframe::egui;
use forge_msgs::image::ImageError;
use forge_msgs::{CompressedImage, Image};

#[derive(Debug)]
enum UiMsg {
    Frame {
        id: String,
        width: u32,
        height: u32,
        rgb: Vec<u8>,
    },
    DoraStopped,
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
    tx: Sender<UiMsg>,
    settings: ResolvedSettings,
    stop: Arc<AtomicBool>,
    decode_warned: Arc<AtomicBool>,
    egui_ctx: egui::Context,
) {
    let wake = || egui_ctx.request_repaint();

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
                    Some((w, h, rgb)) => {
                        let _ = tx.send(UiMsg::Frame {
                            id: id_str,
                            width: w,
                            height: h,
                            rgb,
                        });
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
                let _ = tx.send(UiMsg::DoraStopped);
                wake();
                break;
            }
            Event::Error(msg) => {
                eprintln!("[image_viewer] error: {msg}");
                let _ = tx.send(UiMsg::DoraStopped);
                wake();
                break;
            }
            _ => {}
        }
    }
    let _ = tx.send(UiMsg::DoraStopped);
    wake();
}

fn install_shutdown_signal_handler(
    stop: Arc<AtomicBool>,
    tx: Sender<UiMsg>,
    egui_ctx: egui::Context,
) {
    if let Err(err) = ctrlc::set_handler(move || {
        if !stop.swap(true, Ordering::Relaxed) {
            let _ = tx.send(UiMsg::DoraStopped);
            egui_ctx.request_repaint();
        }
    }) {
        eprintln!("[image_viewer] failed to install shutdown signal handler: {err}");
    }
}

struct ImageViewerApp {
    rx: Receiver<UiMsg>,
    /// 每路最新 RGB 像素（用于生成/更新纹理）
    latest: HashMap<String, (u32, u32, Vec<u8>)>,
    textures: HashMap<String, egui::TextureHandle>,
    stop: Arc<AtomicBool>,
    /// 用户手动关闭的 input 窗口；本次运行中忽略该 input 的后续帧。
    closed_input_ids: HashSet<String>,
}

impl ImageViewerApp {
    fn new(cc: &eframe::CreationContext<'_>, rx: Receiver<UiMsg>, stop: Arc<AtomicBool>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        Self {
            rx,
            latest: HashMap::new(),
            textures: HashMap::new(),
            stop,
            closed_input_ids: HashSet::new(),
        }
    }

    fn drain_updates(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                UiMsg::Frame {
                    id,
                    width,
                    height,
                    rgb,
                } => {
                    if self.closed_input_ids.contains(&id) {
                        continue;
                    }
                    self.latest.insert(id, (width, height, rgb));
                    ctx.request_repaint();
                }
                UiMsg::DoraStopped => {
                    self.stop.store(true, Ordering::Relaxed);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    fn refresh_textures(&mut self, ctx: &egui::Context) {
        for (id, (w, h, rgb)) in &self.latest {
            if *w == 0 || *h == 0 || rgb.is_empty() {
                continue;
            }
            let size = [*w as usize, *h as usize];
            let color_image = egui::ColorImage::from_rgb(size, rgb);
            match self.textures.get_mut(id) {
                Some(tex) => {
                    tex.set(color_image, egui::TextureOptions::LINEAR);
                }
                None => {
                    let tex = ctx.load_texture(
                        format!("img_{id}"),
                        color_image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.textures.insert(id.clone(), tex);
                }
            }
        }
    }

    fn initial_image_window_size(texture_size: egui::Vec2) -> egui::Vec2 {
        texture_size.max(egui::vec2(1.0, 1.0))
    }

    fn sorted_texture_ids(&self) -> Vec<String> {
        let mut ids: Vec<_> = self.textures.keys().cloned().collect();
        ids.sort();
        ids
    }

    fn hide_input_window(&mut self, id: &str) {
        self.latest.remove(id);
        self.textures.remove(id);
        self.closed_input_ids.insert(id.to_owned());
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
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            return;
        };
        let Some(tex) = self.textures.get(id) else {
            return;
        };

        let title = format!("image_viewer:{id}");
        let image_size = Self::initial_image_window_size(tex.size_vec2());
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(image_size));
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(image_size));
        ctx.send_viewport_cmd(egui::ViewportCommand::MaxInnerSize(image_size));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));

        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                ui.image((tex.id(), tex.size_vec2()));
            });
    }

    fn show_image_viewports(&mut self, ctx: &egui::Context, main_id: Option<&str>) {
        let mut to_close = Vec::new();
        for id in self.sorted_texture_ids() {
            if main_id == Some(id.as_str()) {
                continue;
            }
            let Some(tex) = self.textures.get(&id).cloned() else {
                continue;
            };

            let title = format!("image_viewer:{id}");
            let viewport_id = egui::ViewportId::from_hash_of(("image_viewer", &id));
            let image_size = Self::initial_image_window_size(tex.size_vec2());
            let builder = egui::ViewportBuilder::default()
                .with_title(title.clone())
                .with_inner_size(image_size)
                .with_min_inner_size(image_size)
                .with_max_inner_size(image_size)
                .with_resizable(false)
                .with_maximize_button(false);

            let close_requested = ctx.show_viewport_immediate(viewport_id, builder, {
                let title = title.clone();
                move |ctx, class| {
                    let mut close_requested = ctx.input(|i| i.viewport().close_requested());
                    if matches!(class, egui::ViewportClass::Embedded) {
                        let mut open = true;
                        egui::Window::new(title.as_str())
                            .open(&mut open)
                            .resizable(false)
                            .show(ctx, |ui| {
                                ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                                ui.image((tex.id(), tex.size_vec2()));
                            });
                        close_requested |= !open;
                    } else {
                        egui::CentralPanel::default()
                            .frame(egui::Frame::none())
                            .show(ctx, |ui| {
                                ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                                ui.image((tex.id(), tex.size_vec2()));
                            });
                    }
                    close_requested
                }
            });

            if close_requested {
                to_close.push(id);
            }
        }

        for id in to_close {
            self.hide_input_window(&id);
        }
    }
}

impl eframe::App for ImageViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_updates(ctx);
        if self.stop.load(Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        self.refresh_textures(ctx);
        let main_id = self.sorted_texture_ids().into_iter().next();
        self.handle_main_viewport_close(ctx, main_id.as_deref());
        let main_id = self.sorted_texture_ids().into_iter().next();
        self.show_main_viewport(ctx, main_id.as_deref());
        self.show_image_viewports(ctx, main_id.as_deref());
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

    let (tx, rx) = unbounded();
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
            install_shutdown_signal_handler(Arc::clone(&stop_ui), tx.clone(), egui_ctx.clone());
            let stop_bg = Arc::clone(&stop_ui);
            let settings_clone = settings.clone();
            let tx_bg = tx.clone();
            let warned = Arc::clone(&decode_warned);
            std::thread::spawn(move || {
                let _keep_node = node;
                run_dora_thread(events, tx_bg, settings_clone, stop_bg, warned, egui_ctx);
            });
            Ok(Box::new(ImageViewerApp::new(cc, rx, stop_ui)))
        }),
    )
    .map_err(|e| eyre::eyre!("eframe: {e}"))?;

    stop.store(true, Ordering::Relaxed);
    Ok(())
}
