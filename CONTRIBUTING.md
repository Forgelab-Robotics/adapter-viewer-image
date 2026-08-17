# Contributing to Image Viewer

Thank you for contributing to Image Viewer. Keep changes focused, preserve the documented image contracts, and include tests for behavior changes.

## Development setup

The project requires Rust 1.97.1 or newer. Install the Linux desktop development packages listed in `README.md`, then build with:

```bash
cargo build --locked --bin image_viewer
```

The unit and delivery-path tests do not require a camera or desktop session.

## Required checks

Before opening a pull request, run:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked
cargo audit
cargo package --locked
```

For release-packaging changes, also run:

```bash
bash scripts/package_release.sh
```

## Change guidelines

- Keep `Cargo.lock` synchronized with `Cargo.toml`.
- Use only public, reviewable dependency sources.
- Review the dependency tree and licenses whenever dependencies change.
- Add tests for changed decoding, configuration, buffering, or delivery behavior.
- Preserve the documented `forge_msgs.Image`, `CompressedImage`, and legacy-byte semantics.
- Do not commit captures, device identifiers, credentials, private URLs, local paths, generated `target/` content, or `dist/` artifacts.
- Update `README.md` and `CHANGELOG.md` when behavior or user-facing requirements change.

## License

By contributing, you agree that your contributions are licensed under the Apache License, Version 2.0.
