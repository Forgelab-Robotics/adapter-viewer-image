# Changelog

All notable changes to this project are documented in this file.

The project follows Semantic Versioning. Dates use the `YYYY-MM-DD` format.

## Unreleased

## 2.1.1 - 2026-09-15

### Added

- Add Linux ARM64/aarch64 release packaging alongside the existing x86_64 artifact, using the Ubuntu 20.04/glibc 2.31 release baseline.

### Fixed

- Updated the locked `rustls` dependency to 0.23.45 to address `RUSTSEC-2026-0285`.

## 2.1.0 - 2026-09-08

### Added

- Add opt-in publish/receive latency observation with `FORGE_OBSERVABILITY=1`, using the published `forgelab_common 2.1.0` crate. Report bounded hop/end-to-end interval aggregates and diagnostics without including decoding or display time.

### Changed

- Implement latest-only before decoding inside the viewer: a dedicated receive thread replaces each input's pending frame, and a separate decoder selects one pending input at a time with FIFO fairness between ports. Keep the existing latest-frame UI mailbox.
- Bound pending input count and visible Arrow buffer storage, account for shared allocations once per message, discard pending work on stop, and report replacement totals. Keep successful-image admission and bounded decode warnings.
- Keep standard Dora input mappings in examples; no `queue_size` or queue-policy override is required. Intermediate frames may be dropped for freshness, without increasing decode throughput or claiming screen-display latency.

### Fixed

- Apply specific source and Cargo path remaps after the home-directory fallback so release binaries do not retain private build-directory layouts.

## 2.0.0 - 2026-09-04

### Changed

- Migrated the viewer and USB camera example to the Dora 1.x line and Arrow 59; the current lock validates Dora 1.0.1.
- Updated `forge_msgs` to the published Forge 2.0 crates.io release.
- Updated the locked `h2` dependency to 0.4.19 to address `RUSTSEC-2026-0258`.
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
