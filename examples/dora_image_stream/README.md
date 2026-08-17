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

2. 单独构建兼容的 `usb_camera` Dora 节点，并将其加入 `PATH`。
3. 根据实际设备修改 `camera.yaml` 中的 `/dev/video0`。

## 运行

在本目录执行：

```bash
dora run dataflow.yaml
```

`dataflow.yaml` 使用相对路径 `../../target/debug/image_viewer`，因此必须构建 debug 产物。部署 release 包时，可把 `path` 改成已安装到 `PATH` 的 `image_viewer`。
