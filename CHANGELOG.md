# Changelog

All notable changes to this project are documented in this file.

The project follows Semantic Versioning. Dates use the `YYYY-MM-DD` format.

## Unreleased

### Changed

- Migrated the viewer and USB camera example to Dora 1.0 and Arrow 59.
- Updated `forge_msgs` to the coordinated Forge 2.0 candidate at commit `ca23017`.
- Raised the package version to 2.0.0 because Dora 0.x and 1.x nodes cannot interoperate.

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
