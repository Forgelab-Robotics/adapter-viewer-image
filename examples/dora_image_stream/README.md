# Dora Image Stream Example

该示例连接独立的 `usb_camera` 节点与本项目的 `image_viewer`：

```text
USB/UVC camera -> forge_msgs.Image/CompressedImage -> image_viewer
```

## 前置条件

1. 在本项目根目录执行 `cargo build --locked --bin image_viewer` 构建 debug 二进制；
   依赖已发布的 `forgelab_common 2.1.0`，不需要本地 patch，见 [Viewer 构建说明](../../README.md#编译)。
2. 构建已接入 Common `2.1.0` 观测 API 的 `usb_camera`，并将其加入 `PATH`，避免误用旧版二进制。
3. 安装兼容的 Dora CLI 1.x；Dora 0.x 与 1.x 节点不能互通。
4. 根据实际设备修改 `camera.yaml` 中的设备路径，可用 `usb_camera list-devices --json` 查看设备。

## 运行

在本目录执行：

```bash
dora run dataflow.yaml
```

`dataflow.yaml` 使用相对路径 `../../target/debug/image_viewer`，因此必须构建 debug 产物。
部署 release 包时，可把 `path` 改成已安装到 `PATH` 的 `image_viewer`。

## Latest-only 输入

本示例使用普通的 `image: camera/image` 映射，不设置 Dora 的 `queue_size` 或 `queue_policy`。
Viewer 接收线程持续收取输入，在有界 mailbox 中按端口覆盖尚未解码的帧；独立解码线程
只取每路最新待处理帧。解码后的 UI mailbox 也只保留最新帧。

该行为内置于 Viewer，与观测开关无关。处理不过来时允许丢弃中间帧，不保证逐帧交付，
不取消当前正在解码的帧。正常停止时会丢弃待处理帧，因此接收数减去覆盖数不等于显示帧数。

## 延迟观测

在 camera 和 image_viewer 两个节点上分别添加以下配置，再运行 dataflow：

```yaml
env:
  FORGE_OBSERVABILITY: "1"
```

Viewer 每 5 秒输出 hop/E2E 区间聚合、`received` 和 diagnostic counters；
无输入时也会输出空区间，空区间不代表健康的零延迟。

- Camera 以每次发布作为新的 origin，直连 Viewer 时 hop 与 E2E 相同。
- 旧发送端没有观测 metadata 时，Viewer 记录 `missing_context`，不会生成虚假的延迟样本。
- 观测止于 INPUT 接收，不包含解码前 mailbox 等待、图像解码或屏幕显示。
- 更低的 hop 延迟不代表更高的解码吞吐或同样低的显示延迟。
- 移除环境变量或设为 `0` 可关闭观测，不影响 Viewer 内置的 latest-only 行为。
