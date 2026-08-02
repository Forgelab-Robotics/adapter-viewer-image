use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_yaml::Value;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_yaml(path: &Path) -> Value {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_yaml::from_str(&text)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
}

#[test]
fn delivery_files_are_at_stable_paths() {
    let root = root();
    for relative in [
        "config/viewer.example.yaml",
        "examples/dora_image_stream/README.md",
        "examples/dora_image_stream/camera.yaml",
        "examples/dora_image_stream/dataflow.yaml",
        "examples/dora_image_stream/viewer.yaml",
        "scripts/package_release.sh",
    ] {
        assert!(root.join(relative).is_file(), "missing {relative}");
    }
}

#[test]
fn manifest_keeps_stable_package_and_binary_names() {
    let manifest = fs::read_to_string(root().join("Cargo.toml")).expect("read Cargo.toml");
    assert!(manifest.contains("name = \"forge-tools-image-viewer\""));
    assert!(manifest.contains("name = \"image_viewer\""));
}

#[test]
fn version_flag_reports_binary_name_and_package_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_image_viewer"))
        .arg("--version")
        .output()
        .expect("run image_viewer --version");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "image_viewer 1.0.0\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn example_connects_camera_to_viewer_with_relative_artifact_path() {
    let example_dir = root().join("examples/dora_image_stream");
    let dataflow = read_yaml(&example_dir.join("dataflow.yaml"));
    let nodes = dataflow["nodes"]
        .as_sequence()
        .expect("nodes must be a list");
    assert_eq!(nodes.len(), 2);

    let viewer = nodes
        .iter()
        .find(|node| node["id"] == "image_viewer")
        .expect("image_viewer node");
    let viewer_path = viewer["path"].as_str().expect("viewer path");
    assert!(!Path::new(viewer_path).is_absolute());
    assert_eq!(viewer_path, "../../target/debug/image_viewer");
    assert_eq!(viewer["inputs"]["image"], "camera/image");

    let config = read_yaml(&root().join("config/viewer.example.yaml"));
    assert_eq!(config["renderer"], "wgpu");
}

#[test]
fn package_script_matches_delivery_contract() {
    let script =
        fs::read_to_string(root().join("scripts/package_release.sh")).expect("read package script");
    assert!(script.contains("cargo build --release --locked --bin image_viewer"));
    assert!(script.contains("target/release/image_viewer"));
    assert!(script.contains("ARTIFACT=\"${DIST_DIR}/image_viewer\""));
    assert!(!script.contains("tar "));
    assert!(!script.contains("viewer.example.yaml"));
    assert!(!script.contains("README.md"));
}
