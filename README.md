# Image Viewer

独立的 Rust/Dora 实时图像查看工具，基于 `eframe`/`egui` 显示一路或多路图像流。项目从 `forge_runtime` 抽离，作为可独立构建、测试和交付的 Viewer 节点维护。

## 支持范围

- 输入 `forge_msgs.Image`：`rgb8`、`bgr8`、`mono8`、`16UC1`、`32FC1`。
- 输入 `forge_msgs.CompressedImage`：JPEG、PNG（由当前 `forge_msgs` image features 提供）。
- 兼容 legacy raw bytes，可在配置中指定宽、高、通道数及 BGR 排列。
- 每路输入创建独立窗口，关闭某一路窗口后，本次运行会忽略该路后续帧。
- 每路仅缓存最新待显示帧；渲染跟不上输入时主动丢弃旧帧，避免队列积压导致窗口卡死或延迟持续增长。
- 双渲染后端：默认 WGPU（Linux 通常走 Vulkan），可切换到 Glow/OpenGL。

## 项目结构

```text
image_viewer/
├── Cargo.toml
├── Cargo.lock
├── config/
│   └── viewer.example.yaml
├── examples/
│   └── dora_image_stream/
├── scripts/
│   └── package_release.sh
├── src/
│   ├── config.rs
│   └── main.rs
└── tests/
    └── delivery_paths.rs
```

Cargo package 名为 `forge-tools-image-viewer`，交付二进制名保持为 `image_viewer`，因此现有 Dora dataflow 可继续使用：

```yaml
path: image_viewer
```

## 环境要求

- 支持 Rust 2024 edition 的 stable toolchain；建议使用当前稳定版 Rust。
- Dora 运行环境。
- 能访问内部 GitLab `https://gitlab.ex-ai.cn/meta-emt/framework/forge.git`；源码构建需要获取锁文件指定的 `forge_msgs` commit。直接使用 release 压缩包不需要源码仓库权限。
- Linux 桌面环境：X11 或 Wayland。
- WGPU 模式需要可用的 Vulkan/图形驱动；Glow 模式需要可用的 OpenGL/GLX/EGL 驱动。

Ubuntu/Debian 构建环境可安装：

```bash
sudo apt install build-essential pkg-config libx11-dev libxkbcommon-dev \
  libwayland-dev libgl1-mesa-dev libvulkan1
```

使用 NVIDIA GPU 时还需要与当前内核匹配的 NVIDIA 驱动。`nvidia-smi` 失败通常表示驱动层不可用，并非 Viewer 图像解码问题。

## 编译

在项目根目录执行。

开发构建：

```bash
cargo build --locked --bin image_viewer
```

产物：

```text
target/debug/image_viewer
```

Release 构建：

```bash
cargo build --release --locked --bin image_viewer
```

产物：

```text
target/release/image_viewer
```

安装到当前用户的 `PATH`：

```bash
install -Dm755 target/release/image_viewer ~/.local/bin/image_viewer
```

## 打包交付

执行标准打包脚本：

```bash
bash scripts/package_release.sh
```

脚本会使用锁文件进行 release 构建，清理旧 `dist/` 内容，并且只生成一个二进制文件：

```text
dist/image_viewer
```

安装该二进制：

```bash
install -Dm755 dist/image_viewer ~/.local/bin/image_viewer
```

该产物是动态链接的 Linux 二进制；目标机器仍需提供兼容的 glibc、X11/Wayland、Vulkan/OpenGL 驱动及 Dora 运行环境。建议在与部署机相同的 Linux 发行版和 CPU 架构上构建。

## 配置

推荐从交付示例复制：

```bash
cp config/viewer.example.yaml image_viewer.yaml
image_viewer --config image_viewer.yaml
```

配置文件查找顺序：

1. CLI：`--config /path/to/image_viewer.yaml`
2. 环境变量：`IMAGE_VIEWER_CONFIG=/path/to/image_viewer.yaml`
3. 内置默认配置

显式传入的 CLI 或环境变量配置路径不存在、不可读或 YAML 非法时，程序会直接报错，不会静默回退到默认配置。

示例：

```yaml
# 为空或省略时显示除 tick 外的所有输入。
images:
  - image/top
  - image/wrist

# wgpu（默认）或 glow。
renderer: wgpu

# 仅用于 legacy raw bytes。
width: 640
height: 480
channels: 3
bgr: false
```

CLI 参数：

- `--config <PATH>`：配置文件路径。
- `--input-id <ID>`：临时只显示指定 input。
- `--width <W>`、`--height <H>`、`--channels <C>`：显式覆盖 legacy raw bytes 尺寸/通道参数，包括覆盖成默认值 `640/480/3`。
- `--bgr`：强制将 legacy 三通道输入按 BGR 解码。
- `--renderer <wgpu|glow>`：临时覆盖 YAML 中的渲染后端。

渲染后端优先级：

```text
--renderer > YAML renderer > wgpu
```

## Dora 示例

`examples/dora_image_stream/` 提供 `usb_camera -> image_viewer` 联调链路。先构建本项目 debug 二进制，并确保独立的 `usb_camera` 二进制位于 `PATH`：

```bash
cargo build --locked --bin image_viewer
cd examples/dora_image_stream
dora run dataflow.yaml
```

根据设备修改 `camera.yaml`。该示例固定引用 `target/debug/image_viewer`；release 部署建议将 dataflow 中的 path 改为已安装到 `PATH` 的 `image_viewer`。

在其他 dataflow 中使用：

```yaml
nodes:
  - id: image_viewer
    path: image_viewer
    inputs:
      image/top: camera_top/image
      image/wrist: camera_wrist/image
    args: --config image_viewer.yaml
```

## 渲染后端与故障排查

默认 WGPU 初始化失败时可切换到 OpenGL：

```bash
image_viewer --renderer glow
```

诊断硬件 OpenGL 驱动问题时可临时使用 Mesa 软件渲染：

```bash
LIBGL_ALWAYS_SOFTWARE=1 image_viewer --renderer glow
```

如果 NVIDIA 驱动整体失效，WGPU 与 Glow 都可能无法启动，应先检查：

```bash
nvidia-smi
journalctl -k -b | grep -Ei 'nvrm|xid|nvidia'
```

## 测试与交付检查

无需摄像头的测试：

```bash
cargo test --locked
```

格式与静态检查：

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
```

`tests/delivery_paths.rs` 会校验配置、示例、打包脚本命令、归档命名以及稳定的 package/binary 名称，避免交付路径在重构时意外失效。

## 已知限制

- 必须运行在可创建原生窗口的桌面会话中；纯 headless 环境不会自动降级为无界面 sink。
- 同一进程内不能通过再次调用 `eframe::run_native` 自动重建 `winit` EventLoop，因此后端 fallback 由配置或 CLI 显式选择。
- legacy raw bytes 的数据长度必须与 `width * height * channels` 完全一致。
