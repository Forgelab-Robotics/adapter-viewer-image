//! Dora 图像查看节点：Arrow `forge_msgs.Image` / `CompressedImage` + legacy bytes，eframe/egui 显示。

mod config;
mod input_size;
mod latest;
mod observability;

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use arrow_array::{Array, BinaryArray, LargeBinaryArray, RecordBatch, StructArray};
use clap::Parser;
use config::{
    MAX_IMAGE_DIMENSION, MAX_INPUT_ID_LEN, MAX_INPUTS, RendererBackend, ResolvedSettings,
    load_config_file, resolve_settings, validate_image_dimensions,
};

use dora_node_api::{DoraArray, DoraNode, Event, EventStream};
use eframe::egui;
use forge_msgs::{CompressedImage, Image};
use image::{ImageFormat, ImageReader};

const MAX_COMPRESSED_BYTES: usize = 64 * 1024 * 1024;
const MAX_RAW_BYTES: usize = 128 * 1024 * 1024;
const MAX_DECODE_ALLOC_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PENDING_RGB_BYTES: usize = 256 * 1024 * 1024;
const MAX_PENDING_INPUT_BYTES: usize = 256 * 1024 * 1024;
const MAX_TOTAL_TEXTURE_PIXELS: usize = 64 * 1024 * 1024;

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
    fn publish(&self, id: String, frame: ImageFrame) -> bool {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current_bytes = pending
            .values()
            .map(|pending_frame| pending_frame.rgb.len())
            .sum::<usize>();
        let replaced_bytes = pending.get(&id).map_or(0, |old| old.rgb.len());
        let projected_bytes = current_bytes
            .saturating_sub(replaced_bytes)
            .saturating_add(frame.rgb.len());
        if projected_bytes > MAX_PENDING_RGB_BYTES {
            return false;
        }
        pending.insert(id, frame);
        true
    }

    fn take_all(&self) -> HashMap<String, ImageFrame> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *pending)
    }
}

fn arrow_to_record_batch(data: &DoraArray) -> Option<RecordBatch> {
    let arr = data.as_array();
    let sa = arr.as_any().downcast_ref::<StructArray>()?;
    Some(RecordBatch::from(sa.clone()))
}

fn allocate_rgb(pixel_count: usize) -> Result<Vec<u8>, String> {
    let len = pixel_count
        .checked_mul(3)
        .ok_or_else(|| "RGB buffer length overflow".to_owned())?;
    let mut rgb = Vec::new();
    rgb.try_reserve_exact(len)
        .map_err(|error| format!("failed to allocate {len} RGB bytes: {error}"))?;
    Ok(rgb)
}

fn image_row(data: &[u8], y: usize, step: usize, row_len: usize) -> Result<&[u8], String> {
    let start = y
        .checked_mul(step)
        .ok_or_else(|| "row offset overflow".to_owned())?;
    let end = start
        .checked_add(row_len)
        .ok_or_else(|| "row length overflow".to_owned())?;
    data.get(start..end)
        .ok_or_else(|| format!("row {y} exceeds the image buffer"))
}

fn forge_message_kind_and_payload_len(batch: &RecordBatch) -> Result<(bool, usize), String> {
    if batch.num_rows() == 0 {
        return Err("Forge image record batch is empty".to_owned());
    }
    let schema = batch.schema();
    let is_raw = ["height", "width", "encoding", "step", "data"]
        .iter()
        .all(|name| schema.index_of(name).is_ok());
    let is_compressed = ["format", "data"]
        .iter()
        .all(|name| schema.index_of(name).is_ok());
    if is_raw == is_compressed {
        return Err("Forge image schema is missing required fields or is ambiguous".to_owned());
    }

    let data_index = schema
        .index_of("data")
        .map_err(|_| "Forge image schema is missing the data field".to_owned())?;
    let data = batch
        .column(data_index)
        .as_any()
        .downcast_ref::<LargeBinaryArray>()
        .ok_or_else(|| "Forge image data field must be LargeBinary".to_owned())?;
    if data.is_empty() || data.is_null(0) {
        return Err("Forge image data field is empty or null".to_owned());
    }
    let payload_len = usize::try_from(data.value_length(0))
        .map_err(|_| "Forge image payload length exceeds this platform".to_owned())?;
    Ok((is_raw, payload_len))
}

fn try_decode_forge_image(batch: &RecordBatch) -> Result<(u32, u32, Vec<u8>), String> {
    let (is_raw, payload_len) = forge_message_kind_and_payload_len(batch)?;
    let limit = if is_raw {
        MAX_RAW_BYTES
    } else {
        MAX_COMPRESSED_BYTES
    };
    if payload_len > limit {
        return Err(format!(
            "image payload contains {payload_len} bytes, exceeding the {limit}-byte limit"
        ));
    }

    if is_raw {
        let image = Image::from_record_batch(batch).map_err(|error| error.to_string())?;
        let rgb = raw_image_to_rgb(&image)?;
        Ok((image.width, image.height, rgb))
    } else {
        let image = CompressedImage::from_record_batch(batch).map_err(|error| error.to_string())?;
        decode_compressed_image(&image)
    }
}

fn decode_compressed_image(image: &CompressedImage) -> Result<(u32, u32, Vec<u8>), String> {
    if image.data.is_empty() {
        return Err("compressed image data is empty".to_owned());
    }
    if image.data.len() > MAX_COMPRESSED_BYTES {
        return Err(format!(
            "compressed image contains {} bytes, exceeding the {MAX_COMPRESSED_BYTES}-byte limit",
            image.data.len()
        ));
    }

    let mut reader = ImageReader::new(Cursor::new(image.data.as_ref()))
        .with_guessed_format()
        .map_err(|error| format!("failed to inspect compressed image: {error}"))?;
    if !matches!(reader.format(), Some(ImageFormat::Jpeg | ImageFormat::Png)) {
        return Err("compressed image must contain JPEG or PNG data".to_owned());
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC_BYTES);
    reader.limits(limits);

    let decoded = reader
        .decode()
        .map_err(|error| format!("failed to decode compressed image: {error}"))?;
    let width = decoded.width();
    let height = decoded.height();
    validate_image_dimensions(width, height).map_err(|error| error.to_string())?;
    Ok((width, height, decoded.to_rgb8().into_raw()))
}

fn raw_image_to_rgb(image: &Image) -> Result<Vec<u8>, String> {
    let pixel_count =
        validate_image_dimensions(image.width, image.height).map_err(|error| error.to_string())?;
    let width = usize::try_from(image.width).map_err(|error| error.to_string())?;
    let height = usize::try_from(image.height).map_err(|error| error.to_string())?;
    let step = usize::try_from(image.step).map_err(|error| error.to_string())?;
    let data = image.data.as_ref();
    if data.len() > MAX_RAW_BYTES {
        return Err(format!(
            "raw image contains {} bytes, exceeding the {MAX_RAW_BYTES}-byte limit",
            data.len()
        ));
    }

    match image.encoding.as_str() {
        "rgb8" => {
            let row_len = width
                .checked_mul(3)
                .ok_or_else(|| "RGB row length overflow".to_owned())?;
            let mut rgb = allocate_rgb(pixel_count)?;
            for y in 0..height {
                rgb.extend_from_slice(image_row(data, y, step, row_len)?);
            }
            Ok(rgb)
        }
        "bgr8" => {
            let row_len = width
                .checked_mul(3)
                .ok_or_else(|| "BGR row length overflow".to_owned())?;
            let mut rgb = allocate_rgb(pixel_count)?;
            for y in 0..height {
                for pixel in image_row(data, y, step, row_len)?.as_chunks::<3>().0 {
                    rgb.extend([pixel[2], pixel[1], pixel[0]]);
                }
            }
            Ok(rgb)
        }
        "mono8" => {
            let mut rgb = allocate_rgb(pixel_count)?;
            for y in 0..height {
                for &gray in image_row(data, y, step, width)? {
                    rgb.extend([gray, gray, gray]);
                }
            }
            Ok(rgb)
        }
        "16UC1" => {
            let values = read_u16_image(data, width, height, step, pixel_count)?;
            grayscale_u16_to_rgb(&values)
        }
        "32FC1" => {
            let values = read_f32_image(data, width, height, step, pixel_count)?;
            grayscale_f32_to_rgb(&values)
        }
        other => Err(format!("unsupported image encoding `{other}`")),
    }
}

fn read_u16_image(
    data: &[u8],
    width: usize,
    height: usize,
    step: usize,
    pixel_count: usize,
) -> Result<Vec<u16>, String> {
    let row_len = width
        .checked_mul(2)
        .ok_or_else(|| "16UC1 row length overflow".to_owned())?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(pixel_count)
        .map_err(|error| format!("failed to allocate 16UC1 values: {error}"))?;
    for y in 0..height {
        for chunk in image_row(data, y, step, row_len)?.as_chunks::<2>().0 {
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
    pixel_count: usize,
) -> Result<Vec<f32>, String> {
    let row_len = width
        .checked_mul(4)
        .ok_or_else(|| "32FC1 row length overflow".to_owned())?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(pixel_count)
        .map_err(|error| format!("failed to allocate 32FC1 values: {error}"))?;
    for y in 0..height {
        for chunk in image_row(data, y, step, row_len)?.as_chunks::<4>().0 {
            values.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
    }
    Ok(values)
}

fn grayscale_u16_to_rgb(values: &[u16]) -> Result<Vec<u8>, String> {
    let mut rgb = allocate_rgb(values.len())?;
    let max = values.iter().copied().max().unwrap_or(0);
    for &value in values {
        let gray = if max == 0 {
            0
        } else {
            ((value as f32 / max as f32) * 255.0).round() as u8
        };
        rgb.extend([gray, gray, gray]);
    }
    Ok(rgb)
}

fn grayscale_f32_to_rgb(values: &[f32]) -> Result<Vec<u8>, String> {
    let min = values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .fold(f32::INFINITY, f32::min);
    let max = values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .fold(f32::NEG_INFINITY, f32::max);
    let mut rgb = allocate_rgb(values.len())?;
    for &value in values {
        let gray = if value.is_finite() && max > min {
            (((value - min) / (max - min)) * 255.0).clamp(0.0, 255.0) as u8
        } else {
            0
        };
        rgb.extend([gray, gray, gray]);
    }
    Ok(rgb)
}

fn try_decode_legacy_bytes(
    data: &DoraArray,
    settings: &ResolvedSettings,
) -> Result<(u32, u32, Vec<u8>), String> {
    let array = data.as_array();
    let bytes: &[u8] = if let Some(array) = array.as_any().downcast_ref::<LargeBinaryArray>() {
        if array.is_empty() || array.is_null(0) {
            return Err("legacy LargeBinary input is empty or null".to_owned());
        }
        array.value(0)
    } else if let Some(array) = array.as_any().downcast_ref::<BinaryArray>() {
        if array.is_empty() || array.is_null(0) {
            return Err("legacy Binary input is empty or null".to_owned());
        }
        array.value(0)
    } else {
        return Err("input is neither a Forge image struct nor legacy binary data".to_owned());
    };

    let pixel_count = validate_image_dimensions(settings.legacy_width, settings.legacy_height)
        .map_err(|error| error.to_string())?;
    let channels = usize::try_from(settings.legacy_channels).map_err(|error| error.to_string())?;
    let expected = pixel_count
        .checked_mul(channels)
        .ok_or_else(|| "legacy input length overflow".to_owned())?;
    if bytes.len() != expected {
        return Err(format!(
            "legacy input contains {} bytes; expected {expected}",
            bytes.len()
        ));
    }

    let mut rgb = allocate_rgb(pixel_count)?;
    match settings.legacy_channels {
        1 => {
            for &gray in bytes {
                rgb.extend([gray, gray, gray]);
            }
        }
        3 if settings.legacy_bgr => {
            for pixel in bytes.as_chunks::<3>().0 {
                rgb.extend([pixel[2], pixel[1], pixel[0]]);
            }
        }
        3 => rgb.extend_from_slice(bytes),
        channels => return Err(format!("unsupported legacy channel count {channels}")),
    }
    Ok((settings.legacy_width, settings.legacy_height, rgb))
}

fn decode_input(
    data: &DoraArray,
    settings: &ResolvedSettings,
) -> Result<(u32, u32, Vec<u8>), String> {
    match arrow_to_record_batch(data) {
        Some(batch) => try_decode_forge_image(&batch),
        None => try_decode_legacy_bytes(data, settings),
    }
}

fn input_allowed(id: &str, filter: &Option<Vec<String>>) -> bool {
    if id == "tick" || id.is_empty() || id.len() > MAX_INPUT_ID_LEN {
        return false;
    }
    match filter {
        None => true,
        Some(ids) => ids.iter().any(|configured| configured == id),
    }
}

struct CloseDecodeQueue(Arc<latest::LatestMailbox<DoraArray>>);

impl Drop for CloseDecodeQueue {
    fn drop(&mut self) {
        self.0.close();
    }
}

fn run_decode_thread(
    pending: &latest::LatestMailbox<DoraArray>,
    frames: &FrameMailbox,
    settings: &ResolvedSettings,
    stop: &AtomicBool,
    egui_ctx: &egui::Context,
    active_ids: &Mutex<HashSet<String>>,
) {
    let mut decode_warned_ids = HashSet::new();
    let mut decode_warning_limit_warned = false;
    let mut mailbox_limit_warned = false;
    while let Some((id, data)) = pending.take() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        {
            let active = active_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !active.contains(&id) && active.len() >= MAX_INPUTS {
                continue;
            }
        }
        let decoded = decode_input(&data, settings);
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match decoded {
            Ok((width, height, rgb)) => {
                active_ids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(id.clone());
                if frames.publish(id, ImageFrame { width, height, rgb }) {
                    egui_ctx.request_repaint_of(egui::ViewportId::ROOT);
                } else if !mailbox_limit_warned {
                    eprintln!(
                        "[image_viewer] dropping frames after reaching the {MAX_PENDING_RGB_BYTES}-byte pending-frame limit"
                    );
                    mailbox_limit_warned = true;
                }
            }
            Err(error) => {
                if decode_warned_ids.len() < MAX_INPUTS {
                    if decode_warned_ids.insert(id.clone()) {
                        eprintln!("[image_viewer] failed to decode input `{id}`: {error}");
                    }
                } else if !decode_warning_limit_warned {
                    eprintln!(
                        "[image_viewer] suppressing decode warnings after {MAX_INPUTS} distinct failing input IDs"
                    );
                    decode_warning_limit_warned = true;
                }
            }
        }
    }
}

fn run_dora_thread(
    mut events: EventStream,
    frames: Arc<FrameMailbox>,
    settings: ResolvedSettings,
    stop: Arc<AtomicBool>,
    egui_ctx: egui::Context,
    observer: Option<Arc<forge_common::observability::Observer>>,
) {
    let wake = || egui_ctx.request_repaint_of(egui::ViewportId::ROOT);
    let pending = Arc::new(latest::LatestMailbox::new(
        MAX_INPUTS,
        MAX_PENDING_INPUT_BYTES,
    ));
    // Wake the decoder even if receiving unwinds before normal shutdown.
    let close_pending = CloseDecodeQueue(Arc::clone(&pending));
    let active_ids = Arc::new(Mutex::new(HashSet::new()));
    let decoder = {
        let pending = Arc::clone(&pending);
        let stop = Arc::clone(&stop);
        let ctx = egui_ctx.clone();
        let settings = settings.clone();
        let active_ids = Arc::clone(&active_ids);
        std::thread::Builder::new()
            .name("image-viewer-decode".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_decode_thread(&pending, &frames, &settings, &stop, &ctx, &active_ids);
                }));
                if result.is_err() {
                    stop.store(true, Ordering::Relaxed);
                    pending.close();
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                    eprintln!("[image_viewer] decode worker thread panicked");
                }
            })
    };
    let decoder = match decoder {
        Ok(decoder) => decoder,
        Err(error) => {
            stop.store(true, Ordering::Relaxed);
            wake();
            eprintln!("[image_viewer] failed to start decode worker: {error}");
            return;
        }
    };

    let mut input_limit_warned = false;
    let mut pending_limit_warned = false;
    let mut replaced = 0_u64;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let event = match events.recv() {
            Some(e) => e,
            None => break,
        };
        match event {
            Event::Input { id, metadata, data } => {
                if let Some(observer) = &observer {
                    observability::observe(observer.as_ref(), id.as_str(), &metadata.parameters);
                }
                if !input_allowed(id.as_str(), &settings.filter_ids) {
                    continue;
                }
                let at_input_limit = {
                    let active = active_ids
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    !active.contains(id.as_str()) && active.len() >= MAX_INPUTS
                };
                if at_input_limit {
                    if !input_limit_warned {
                        eprintln!(
                            "[image_viewer] ignoring additional inputs after reaching the {MAX_INPUTS}-input limit"
                        );
                        input_limit_warned = true;
                    }
                    continue;
                }
                let bytes = input_size::retained_buffer_bytes(data.as_array());
                let id = id.as_str().to_owned();
                match pending.publish(id.clone(), data, bytes) {
                    latest::PublishResult::Queued => {}
                    latest::PublishResult::Replaced => {
                        replaced = replaced.saturating_add(1);
                    }
                    latest::PublishResult::Rejected => {
                        if !pending_limit_warned {
                            eprintln!(
                                "[image_viewer] dropping inputs after reaching the {MAX_PENDING_INPUT_BYTES}-byte undecoded-frame limit"
                            );
                            pending_limit_warned = true;
                        }
                    }
                    latest::PublishResult::Closed => break,
                }
            }
            Event::Stop(_) => break,
            Event::Error(msg) => {
                eprintln!("[image_viewer] error: {msg}");
                break;
            }
            _ => {}
        }
    }
    stop.store(true, Ordering::Relaxed);
    drop(close_pending);
    eprintln!("[image_viewer] latest-only replaced_before_decode={replaced}");
    wake();
    // Notify the UI before waiting for in-flight work; never decode the backlog on stop.
    let _ = decoder.join();
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
    texture_limit_warned: bool,
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
            texture_limit_warned: false,
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
            let other_texture_pixels = self
                .textures
                .iter()
                .filter(|(texture_id, _)| texture_id.as_str() != id)
                .map(|(_, texture)| texture.size()[0].saturating_mul(texture.size()[1]))
                .fold(0usize, usize::saturating_add);
            let projected_texture_pixels =
                other_texture_pixels.saturating_add(size[0].saturating_mul(size[1]));
            if projected_texture_pixels > MAX_TOTAL_TEXTURE_PIXELS {
                if !self.texture_limit_warned {
                    eprintln!(
                        "[image_viewer] dropping frames after reaching the {MAX_TOTAL_TEXTURE_PIXELS}-pixel texture limit"
                    );
                    self.texture_limit_warned = true;
                }
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

#[derive(Debug, Parser)]
#[command(
    name = "image_viewer",
    version,
    about = "Display Dora Forge image streams in native windows"
)]
struct CliArgs {
    #[arg(long, value_name = "PATH")]
    config: Option<String>,
    #[arg(long, value_name = "ID")]
    input_id: Option<String>,
    #[arg(long, value_name = "PIXELS")]
    width: Option<u32>,
    #[arg(long, value_name = "PIXELS")]
    height: Option<u32>,
    #[arg(long, value_name = "COUNT")]
    channels: Option<u32>,
    #[arg(long)]
    bgr: bool,
    #[arg(long, value_enum)]
    renderer: Option<RendererBackend>,
}

fn run(cli: CliArgs) -> eyre::Result<()> {
    let file_cfg = load_config_file(cli.config.as_deref())?;
    let settings = resolve_settings(
        file_cfg,
        cli.input_id,
        cli.width,
        cli.height,
        cli.channels,
        cli.bgr,
        cli.renderer,
    )?;
    let renderer = match settings.renderer {
        RendererBackend::Wgpu => eframe::Renderer::Wgpu,
        RendererBackend::Glow => eframe::Renderer::Glow,
    };

    let (node, events) = DoraNode::init_from_env()?;
    let observation = observability::Observation::from_env();
    let observer = observation
        .as_ref()
        .map(observability::Observation::observer);

    let frames = Arc::new(FrameMailbox::default());
    let stop = Arc::new(AtomicBool::new(false));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("X-ERA image_viewer")
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
            let worker = std::thread::Builder::new()
                .name("image-viewer-dora".to_owned())
                .spawn(move || {
                    let _keep_node = node;
                    let panic_stop = Arc::clone(&stop_bg);
                    let panic_ctx = egui_ctx.clone();
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run_dora_thread(
                            events,
                            frames_bg,
                            settings_clone,
                            stop_bg,
                            egui_ctx,
                            observer,
                        );
                    }));
                    if result.is_err() {
                        eprintln!("[image_viewer] Dora worker thread panicked");
                        panic_stop.store(true, Ordering::Relaxed);
                        panic_ctx.request_repaint_of(egui::ViewportId::ROOT);
                    }
                });
            if let Err(error) = worker {
                return Err(Box::new(error));
            }
            Ok(Box::new(ImageViewerApp::new(cc, frames, stop_ui)))
        }),
    )
    .map_err(|e| eyre::eyre!("eframe: {e}"))?;

    stop.store(true, Ordering::Relaxed);
    Ok(())
}

fn main() {
    if let Err(error) = run(CliArgs::parse()) {
        eprintln!("[image_viewer] error: {error:#}");
        std::process::exit(1);
    }
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
        assert!(mailbox.publish("left".to_owned(), frame(1)));
        assert!(mailbox.publish("right".to_owned(), frame(2)));
        assert!(mailbox.publish("left".to_owned(), frame(3)));

        let pending = mailbox.take_all();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending["left"].rgb, vec![3; 3]);
        assert_eq!(pending["right"].rgb, vec![2; 3]);
        assert!(mailbox.take_all().is_empty());
    }

    #[test]
    fn bgr_rows_with_padding_are_converted_safely() {
        let image = Image::new(1, 2, "bgr8", 8, vec![3, 2, 1, 6, 5, 4, 99, 99].into()).unwrap();
        assert_eq!(raw_image_to_rgb(&image).unwrap(), vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn mono_rows_are_expanded_to_rgb() {
        let image = Image::new(1, 2, "mono8", 2, vec![7, 9].into()).unwrap();
        assert_eq!(raw_image_to_rgb(&image).unwrap(), vec![7, 7, 7, 9, 9, 9]);
    }

    #[test]
    fn depth_values_are_scaled_to_the_frame_maximum() {
        let image = Image::new(1, 3, "16UC1", 6, vec![0, 0, 100, 0, 200, 0].into()).unwrap();
        assert_eq!(
            raw_image_to_rgb(&image).unwrap(),
            vec![0, 0, 0, 128, 128, 128, 255, 255, 255]
        );
    }

    #[test]
    fn non_finite_float_pixels_render_as_black() {
        let mut bytes = Vec::new();
        for value in [f32::NAN, 1.0, 3.0] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        let image = Image::new(1, 3, "32FC1", 12, bytes.into()).unwrap();
        assert_eq!(
            raw_image_to_rgb(&image).unwrap(),
            vec![0, 0, 0, 0, 0, 0, 255, 255, 255]
        );
    }

    #[test]
    fn oversized_raw_images_are_rejected_before_allocation() {
        let image = Image {
            width: u32::MAX,
            height: 1,
            encoding: "rgb8".to_owned(),
            step: 0,
            data: Vec::new().into(),
        };
        let error = raw_image_to_rgb(&image).unwrap_err();
        assert!(error.contains("exceed the maximum"));
    }

    #[test]
    fn forge_payload_size_is_checked_before_message_conversion() {
        let raw = Image::new(1, 1, "rgb8", 3, vec![1, 2, 3].into())
            .unwrap()
            .to_record_batch()
            .unwrap();
        assert_eq!(forge_message_kind_and_payload_len(&raw).unwrap(), (true, 3));

        let compressed = CompressedImage::new("jpeg", vec![1, 2, 3, 4].into())
            .unwrap()
            .to_record_batch()
            .unwrap();
        assert_eq!(
            forge_message_kind_and_payload_len(&compressed).unwrap(),
            (false, 4)
        );
    }
}
