# Dora Image Stream Example

该示例连接独立的 `usb_camera` 节点与本项目的 `image_viewer`：

```text
USB/UVC camera -> forge_msgs.Image/CompressedImage -> image_viewer
```

## 前置条件

1. 构建本项目的 debug 二进制：

   ```bash
   cargo build --locked --bin image_viewer
   ```

2. 从 USB Camera 的 Dora 1.0 migration 分支构建 `usb_camera`，并将其加入 `PATH`。
3. 安装兼容的 Dora CLI 1.x（当前验证基线为 1.0.1）；Dora 0.x 与 1.x 节点不能互通。
4. 根据实际设备修改 `camera.yaml` 中的 `/dev/video0`。

## 运行

在本目录执行：

```bash
dora run dataflow.yaml
```

`dataflow.yaml` 使用相对路径 `../../target/debug/image_viewer`，因此必须构建 debug 产物。部署 release 包时，可把 `path` 改成已安装到 `PATH` 的 `image_viewer`。
