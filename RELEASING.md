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

The Dora 1.0 migration branch is not publishable while `forge_msgs` is pinned to
Forge commit `ca23017`. Publish `forge_msgs 2.0.0`, replace the Git dependency with
the crates.io release, regenerate `Cargo.lock`, and restore successful full
`cargo package --locked` verification before releasing Image Viewer 2.0.0.

Dora 1.0.0 currently resolves `lz4_flex 0.10.0`; the affected compression path
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

Build the Linux x86_64 binary with the standard script:

```bash
bash scripts/package_release.sh
```

The script fixes the `x86_64-unknown-linux-gnu` target, remaps local build paths, strips symbols, verifies the ELF architecture, and writes only `dist/image_viewer`.

The executable is dynamically linked. The selected release baseline is glibc 2.39. Before upload, use `file`, `readelf`, and `ldd` to reject missing libraries and unexpected `RPATH`/`RUNPATH` entries, and confirm the highest GLIBC symbol requirement is `GLIBC_2.39`.

The minimal public archive is named `image_viewer-v<version>-linux-x86_64-glibc2.39.tar.gz` and contains only the stripped `image_viewer` executable. Project documentation and licensing remain available in the repository and GitHub-generated source archives.

Create the archive with normalized owner, group, mode, ordering, and timestamp. Scan the final binary for private paths and internal URLs, publish its SHA-256 digest, and smoke-test the extracted binary before publishing.
