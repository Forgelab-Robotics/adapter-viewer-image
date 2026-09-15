use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_yaml::Value;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn run_image_viewer(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_image_viewer"))
        .args(args)
        .output()
        .expect("run image_viewer")
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
        ".github/workflows/ci.yml",
        "LICENSE",
        "CHANGELOG.md",
        "CONTRIBUTING.md",
        "RELEASING.md",
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
    assert!(manifest.contains("license = \"Apache-2.0\""));
    assert!(manifest.contains("authors = [\"X-ERA\"]"));
    assert!(manifest.contains("forge_msgs = \"2.0.0\""));
    assert!(!manifest.contains("git ="));
    assert!(
        manifest
            .contains("repository = \"https://github.com/Forgelab-Robotics/adapter-viewer-image\"")
    );
    assert!(!manifest.contains(concat!("gitlab.", "ex-ai.cn")));
}

#[test]
fn version_flag_reports_binary_name_and_package_version() {
    let output = run_image_viewer(&["--version"]);

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("image_viewer {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn help_is_available_without_a_dora_runtime() {
    let output = run_image_viewer(&["--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Display Dora Forge image streams"));
    assert!(stdout.contains("--renderer <RENDERER>"));
    assert!(output.stderr.is_empty());
}

#[test]
fn invalid_cli_values_are_rejected_before_dora_initialization() {
    for args in [
        &["--width", "nope"][..],
        &["--unknown"][..],
        &["--config"][..],
    ] {
        let output = run_image_viewer(args);
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("error:"));
        assert!(!stderr.contains("DORA_NODE_CONFIG"));
    }
}

#[test]
fn explicit_missing_config_has_a_stable_error_without_debug_locations() {
    let output = run_image_viewer(&[
        "--config",
        "/__forge_image_viewer_missing__/viewer.example.yaml",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("failed to read config"));
    assert!(!stderr.contains("Location:"));
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
fn viewer_example_does_not_require_transport_queue_overrides() {
    let dataflow = read_yaml(&root().join("examples/dora_image_stream/dataflow.yaml"));
    let viewer = dataflow["nodes"]
        .as_sequence()
        .unwrap()
        .iter()
        .find(|node| node["id"] == "image_viewer")
        .unwrap();
    let inputs = viewer["inputs"].as_mapping().expect("viewer inputs");
    assert!(!inputs.is_empty());
    for (id, input) in inputs {
        assert!(
            input.as_str().is_some(),
            "input {id:?} must use default Dora queue settings"
        );
    }
}

#[test]
fn package_script_matches_delivery_contract() {
    let script =
        fs::read_to_string(root().join("scripts/package_release.sh")).expect("read package script");
    assert!(
        script.contains("cargo build --release --locked --target \"${TARGET}\" --bin image_viewer")
    );
    assert!(
        script.contains("BUILT_ARTIFACT=\"${ROOT_DIR}/target/${TARGET}/release/image_viewer\"")
    );
    assert!(script.contains("TARGET=\"${TARGET:-x86_64-unknown-linux-gnu}\""));
    assert!(script.contains("x86_64-unknown-linux-gnu)"));
    assert!(script.contains("aarch64-unknown-linux-gnu)"));
    assert!(script.contains("ERROR: unsupported TARGET"));
    assert!(script.contains("exit 2"));
    assert!(script.contains("FILE_ARCH_REGEX='ELF 64-bit LSB.*x86-64'"));
    assert!(script.contains("FILE_ARCH_REGEX='ELF 64-bit LSB.*ARM aarch64'"));
    assert!(script.contains("READELF_MACHINE='Advanced Micro Devices X86-64'"));
    assert!(script.contains("READELF_MACHINE='AArch64'"));
    assert!(script.contains("file \"${BUILT_ARTIFACT}\" | grep -Eq \"${FILE_ARCH_REGEX}\""));
    assert!(script.contains("Machine:[[:space:]]*${READELF_MACHINE}"));
    assert!(script.contains("trap 'rm -rf \"${DIST_DIR}\"' ERR"));
    assert!(
        script.find("rm -rf \"${DIST_DIR}\"").unwrap()
            < script.find("cargo build --release").unwrap()
    );
    assert!(script.contains("unset CARGO_BUILD_TARGET CARGO_ENCODED_RUSTFLAGS"));
    let home_remap = script
        .find("--remap-path-prefix=${HOME_DIR}=/build")
        .unwrap();
    let cargo_remap = script
        .find("--remap-path-prefix=${CARGO_HOME_DIR}=/cargo")
        .unwrap();
    let source_remap = script.find("--remap-path-prefix=${ROOT_DIR}=.").unwrap();
    assert!(home_remap < cargo_remap && home_remap < source_remap);
    assert!(script.contains("ARTIFACT=\"${DIST_DIR}/image_viewer\""));
    assert!(script.contains("install -m 0755"));
    assert!(!script.contains("tar "));
    assert!(!script.contains("viewer.example.yaml"));
    assert!(!script.contains("README.md"));
}
