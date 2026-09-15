# Changelog

All notable changes to this project are documented in this file.

The project follows Semantic Versioning. Dates use the `YYYY-MM-DD` format.

## Unreleased

## 1.0.2 - 2026-09-15

### Added

- Add Linux ARM64/aarch64 release packaging alongside the existing x86_64 artifact, using the Ubuntu 20.04/glibc 2.31 release baseline.

### Fixed

- Updated the locked `h2` dependency to 0.4.19 to address `RUSTSEC-2026-0258`.

## 1.0.1 - 2026-08-17

### Changed

- Replaced the private `forge_msgs` Git dependency with the public crates.io package.
- Added strict CLI/configuration validation and bounded image decoding.
- Added Apache-2.0 project licensing and public package metadata.
- Added formatting, Clippy, test, package, and dependency-audit CI gates.

### Fixed

- Prevented integer overflow and excessive allocations from malformed image dimensions.
- Made unknown, missing, and malformed command-line arguments fail before Dora initialization.

## 1.0.0 - 2026-08-02

### Added

- Initial standalone Dora image viewer with raw and compressed Forge image support.
- WGPU and Glow rendering backends.
- Multi-input latest-frame mailboxes and native image viewports.
