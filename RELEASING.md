# Releasing Image Viewer

## Versioning

The project follows Semantic Versioning. Update the package version in `Cargo.toml`, update version-sensitive checks, regenerate `Cargo.lock` when required, and move relevant entries from `Unreleased` into a dated section in `CHANGELOG.md`.

The public repository is <https://github.com/Forgelab-Robotics/adapter-viewer-image> and releases are cut from `master`.

The first public release must use a new version after `1.0.0`; the existing private `v1.0.0` tag must not be reused or moved.

## Release checks

Release from a clean, reviewed commit using Rust 1.97.1 or newer:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked
cargo audit --ignore RUSTSEC-2026-0041
cargo package --locked
bash scripts/package_release.sh
```

Complete and record an applicable Dora image-stream smoke test using both the default WGPU renderer and the Glow fallback when supported by the release host.

Image Viewer 2.1.0 resolves the published `forge_msgs 2.0.0` and
`forgelab_common 2.1.0` crates from crates.io. Keep `Cargo.lock` on registry
sources and require successful release-package verification before creating
the release tag. This application is distributed through source tags and
GitHub binary assets, not crates.io.

The Dora 1.0.1 lock currently resolves `lz4_flex 0.10.0`; the affected compression path
is not enabled in this build, so RustSec uses the same targeted
`RUSTSEC-2026-0041` exception as the USB Camera migration.

## Source release

Before creating a tag:

1. Confirm `Cargo.toml`, `Cargo.lock`, `README.md`, and `CHANGELOG.md` agree on the version and requirements.
2. Confirm the package archive contains `LICENSE` and no private files or generated artifacts.
3. Confirm the lock file uses only public dependency sources.
4. Confirm the repository working tree is clean and CI passes.
5. Create an immutable annotated tag named `v<version>` at the validated commit.

Published tags and assets must never be replaced. Any changed payload requires a new version.

## Binary release

The standard script defaults to Linux x86_64 and also accepts the Linux ARM64 target:

```bash
bash scripts/package_release.sh
TARGET=aarch64-unknown-linux-gnu bash scripts/package_release.sh
```

Only `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` are accepted. The script remaps local build paths, strips symbols, verifies the selected ELF architecture with `file` and `readelf`, and writes only `dist/image_viewer`. Build each target on a matching native architecture unless a reviewed cross-compilation environment provides all required system libraries.

The `Build Ubuntu 20.04 binary` workflow runs one matrix invocation that builds both architectures on native runners inside `ubuntu:20.04` containers. Its two minimal public archives are:

- `image_viewer-v<version>-ubuntu20.04-x86_64.tar.gz`
- `image_viewer-v<version>-ubuntu20.04-arm64.tar.gz`

Each archive contains only the stripped `image_viewer` executable and has a matching `.sha256` file. Project documentation and licensing remain available in the repository and GitHub-generated source archives.

The executables are dynamically linked. The release baseline is Ubuntu 20.04/glibc 2.31. For each architecture, the workflow rejects an unexpected `readelf` machine, missing `ldd` libraries, GLIBC symbol requirements newer than 2.31, and unexpected private paths or internal URLs. It also checks the normalized archive and digest, extracts the archive into a clean directory, and runs `image_viewer --version` with a minimal environment.

Before publishing, perform a clean-runtime test of each extracted archive on a matching x86_64 or ARM64 Ubuntu 20.04/glibc 2.31 system with only the documented runtime libraries and graphics stack installed. Recheck `file`, `readelf`, `ldd`, and `RPATH`/`RUNPATH`, then complete the WGPU and Glow smoke tests where supported. Published assets are immutable; do not replace a differing archive or checksum.
