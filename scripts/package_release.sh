#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="${ROOT_DIR}/dist"
ARTIFACT="${DIST_DIR}/image_viewer"

cd "${ROOT_DIR}"
cargo build --release --locked --bin image_viewer

rm -rf "${DIST_DIR}"
mkdir -p "${DIST_DIR}"
install -m 0755 "${ROOT_DIR}/target/release/image_viewer" "${ARTIFACT}"
printf 'Packaged %s\n' "${ARTIFACT}"
