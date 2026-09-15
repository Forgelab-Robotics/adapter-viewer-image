# Image Viewer

独立的 Rust/Dora 实时图像查看工具，基于 `eframe`/`egui` 显示一路或多路图像流。

当前版本：`2.1.1`。

## 支持范围

- 输入 `forge_msgs.Image`：`rgb8`、`bgr8`、`mono8`、`16UC1`、`32FC1`。
- 输入 `forge_msgs.CompressedImage`：JPEG、PNG（使用受资源限制的 `image` 解码器）。
- 兼容 legacy raw bytes，可在配置中指定宽、高、通道数及 BGR 排列。
- 每路输入创建独立窗口，关闭某一路窗口后，本次运行会忽略该路后续帧。
- Viewer 内置 latest-only：接收线程在解码前按 input 覆盖旧的待处理帧，独立解码线程只取每路最新帧；UI mailbox 仍只保留每路最新待显示帧，不依赖 Dora 队列配置。
- 双渲染后端：默认 WGPU（Linux 通常走 Vulkan），可切换到 Glow/OpenGL。

## 项目结构

```text
image_viewer/
├── Cargo.toml
├── Cargo.lock
├── LICENSE
├── CHANGELOG.md
├── CONTRIBUTING.md
├── RELEASING.md
├── config/
│   └── viewer.example.yaml
├── examples/
│   └── dora_image_stream/
├── scripts/
│   └── package_release.sh
├── src/
│   ├── config.rs
│   ├── input_size.rs
│   ├── latest.rs
│   ├── observability.rs
│   └── main.rs
└── tests/
    └── delivery_paths.rs
```

Cargo package 名为 `forge-tools-image-viewer`，交付二进制名保持为 `image_viewer`，因此现有 Dora dataflow 可继续使用：

```yaml
path: image_viewer
```

## 环境要求

- Rust 1.97.1 或更新的 stable 工具链。
- Dora 运行环境。
- 能访问 crates.io 以获取 `Cargo.lock` 固定的公开依赖；准备好 Cargo 缓存后可使用 `--offline` 构建。
- Linux x86_64 或 ARM64/aarch64 桌面环境：X11 或 Wayland。
- WGPU 模式需要可用的 Vulkan/图形驱动；Glow 模式需要可用的 OpenGL/GLX/EGL 驱动。

Ubuntu/Debian 构建环境可安装：

```bash
sudo apt install binutils build-essential file pkg-config libx11-dev libxkbcommon-dev \
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

执行标准打包脚本。未设置 `TARGET` 时默认构建 Linux x86_64：

```bash
bash scripts/package_release.sh
```

在 Linux ARM64/aarch64 主机上构建 ARM64 产物：

```bash
TARGET=aarch64-unknown-linux-gnu bash scripts/package_release.sh
```

脚本仅接受 `x86_64-unknown-linux-gnu` 和 `aarch64-unknown-linux-gnu`，使用锁文件进行 release 构建，清理旧 `dist/` 内容，并且只生成用户二进制：

```text
dist/image_viewer
```

安装该二进制：

```bash
install -Dm755 dist/image_viewer ~/.local/bin/image_viewer
```

两种架构的产物都是动态链接的 Linux 二进制。官方发布工作流在对应架构的原生 runner 上使用 Ubuntu 20.04 容器构建，发布基线为 glibc 2.31；部署系统需要兼容 glibc 2.31，并提供产物所需的动态库、X11/Wayland、Vulkan/OpenGL 驱动及 Dora 运行环境。动态依赖不会打入归档，通常包括系统 C/C++ 运行库、X11/Wayland 与 `libxkbcommon`，以及所选渲染后端使用的 Vulkan loader 或 OpenGL/EGL 库；具体清单以目标机器上的 `ldd dist/image_viewer` 为准。打包脚本使用 `file`、`readelf` 验证目标架构；发布前还需检查 `RPATH`/`RUNPATH`、`ldd` 报告的缺失动态库及最高 GLIBC 符号要求。

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

显式传入的 CLI 或环境变量配置路径不存在、不可读、YAML 非法或包含未知字段时，程序会直接报错，不会静默回退到默认配置。宽、高、通道数、重复 input 等非法值也会在 Dora 初始化前被拒绝。

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

CLI 参数可通过 `image_viewer --help` 查看：

- `--version`：输出 `image_viewer` 版本并退出。
- `--config <PATH>`：配置文件路径。
- `--input-id <ID>`：临时只显示指定 input。
- `--width <W>`、`--height <H>`、`--channels <C>`：显式覆盖 legacy raw bytes 尺寸/通道参数，包括覆盖成默认值 `640/480/3`。
- `--bgr`：强制将 legacy 三通道输入按 BGR 解码。
- `--renderer <wgpu|glow>`：临时覆盖 YAML 中的渲染后端。

渲染后端优先级：

```text
--renderer > YAML renderer > wgpu
```

## 可选延迟观测

仅 `FORGE_OBSERVABILITY=1` 启用。Viewer 在 Dora INPUT 到达后、过滤/解码/mailbox
之前读取 Forge v1 metadata，统计 `forge_hop_latency_seconds` 和
`forge_e2e_latency_seconds`。计数包含之后被过滤或解码失败的 INPUT，不能当作显示帧数。
没有 metadata 的旧发送端仍可用，只记录 `missing_context`，不伪造零延迟。

- 摄像头和 Viewer 均需开启；摄像头以本次 publish 为 origin，因此直连时 hop 与 E2E 相同。
- 观测不包含相机采集到 publish 的时间，也不包含 Viewer 的解码、排队、纹理更新或物理显示。
- 每 5 秒在独立线程向 stdout 输出区间聚合和有界诊断，输入停止时仍输出空区间。
- 指标名称遵循 seconds 约定，但本地文本的 `sum_ns/min_ns/max_ns/le_ns` 明确使用纳秒；
  平均毫秒为 `sum_ns / count / 1_000_000`。桶是累积上界，不能从中声称精确 P99。
- 退出时尝试导出最后区间，最多等待 250 ms；慢/断开的日志读取端可能导致日志丢失。
  这不是可靠持久化 exporter，也没有端到端性能预算保证。
- 默认关闭，不创建观测线程、不读取观测时钟。该开关不影响 Arrow schema 和业务处理。

本项目依赖 crates.io 上的 `forgelab_common >=2.1.0,<3`，`Cargo.lock` 固定具体版本，
不需要本地 Common 源码或 patch。无论是否启用运行时观测，都使用相同构建方式：

```bash
cargo build --locked --bin image_viewer
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
```

实际 dataflow 的观测配置见
[`examples/dora_image_stream/README.md`](examples/dora_image_stream/README.md#延迟观测)。

## 资源限制

为避免异常消息导致整数溢出、无界内存分配或过量原生窗口，运行时执行以下限制：

- 单边最大 `8192` 像素，且总像素数不超过 `33,554,432`。
- 原始图像数据最大 `128 MiB`；压缩输入最大 `64 MiB`。
- 解码前最多 `16` 路待处理帧，每路一帧，可见 Arrow backing buffers 的估算总量上限 `256 MiB`；同一消息中的共享分配去重计量。外部 allocator 的隐藏容量不可知，因此这不是精确 RSS 上限。正在解码的一帧独立于待处理预算。
- JPEG/PNG 解码器分配预算最大 `128 MiB`。
- 每个进程最多接受 `16` 个成功解码的不同 input ID，ID 最长 `256` 字节。
- 待显示 RGB mailbox 总量不超过 `256 MiB`，现存纹理总量不超过 `67,108,864` 像素。
- legacy raw bytes 仅接受 `1` 或 `3` 通道，数据长度必须与配置完全一致。

超出限制的帧会被拒绝；同一路 input 的首个解码错误会写入标准错误。

## Dora 示例

`examples/dora_image_stream/` 提供 `usb_camera -> image_viewer` 联调链路。先构建本项目 debug 二进制，并确保独立的 `usb_camera` 二进制位于 `PATH`：

```bash
cargo build --locked --bin image_viewer
cd examples/dora_image_stream
dora run dataflow.yaml
```

根据设备修改 `camera.yaml`。该示例固定引用 `target/debug/image_viewer`；release 部署建议将 dataflow 中的 path 改为已安装到 `PATH` 的 `image_viewer`。

### Latest-only 预览策略

预览优先显示新画面，不保证每帧都被处理。Viewer 自身实现两级有界缓存：

```text
Dora INPUT → 接收/观测 → 每路最新未解码帧 → 独立解码线程 → 每路最新 RGB 帧 → UI
```

接收线程不做 JPEG/PNG 解码，按 input ID 覆盖待处理帧。解码线程每次只取一个输入；
其他输入仍保留在队列中，可以继续被新帧覆盖。替换不改变该输入的排队位置，因此高频一路
不会饿死已在等待的其他路。正在解码的一帧不会被强制取消。

此前只有解码后的 mailbox 是 latest-only，接收线程还同步解码，旧消息因而积压在 Dora
队列里。现在 latest-only 是 Viewer 的固有行为，使用普通 `image: camera/image` 映射即可，
无需 `queue_size`/`queue_policy`，也与 `FORGE_OBSERVABILITY` 开关无关。

正常停止时丢弃未解码帧、唤醒解码线程，并尽力打印累计 `replaced_before_decode`。
观测的 hop/E2E 仍止于接收边界，不包含新增的解码前等待、解码或显示时间；更低的 hop
不等于更高的解码 FPS，也不等于同样低的屏幕延迟。该策略不适用于完整逐帧录制。
Dora 自身仍有传输缓冲；如果接收线程本身也处理不过来，Viewer 无法消除上游积压。

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

格式、静态检查与依赖审计：

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo audit
```

`tests/delivery_paths.rs` 会校验严格 CLI 行为、配置、示例、打包脚本及稳定的 package/binary 名称，避免交付路径在重构时意外失效。

## 已知限制

- 必须运行在可创建原生窗口的桌面会话中；纯 headless 环境不会自动降级为无界面 sink。
- 同一进程内不能通过再次调用 `eframe::run_native` 自动重建 `winit` EventLoop，因此后端 fallback 由配置或 CLI 显式选择。
- legacy raw bytes 的数据长度必须与 `width * height * channels` 完全一致。
- 后台 Dora 接收由 Dora event stream 驱动；进程退出时，操作系统负责终止仍阻塞在接收调用中的后台线程。

## 许可证

本项目由 X-ERA 以 Apache License 2.0 发布，完整文本见 `LICENSE`。依赖许可证在依赖变更和发布 review 中审计；项目许可证与源码材料由仓库及 GitHub 自动生成的 source archive 提供。
