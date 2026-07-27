#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PACKAGE_NAME="forge-tools-image-viewer"
ARCH="$(uname -m)"
DIST_DIR="${ROOT_DIR}/dist"
STAGE_DIR="${DIST_DIR}/${PACKAGE_NAME}"
ARCHIVE="${DIST_DIR}/${PACKAGE_NAME}-linux-${ARCH}.tar.gz"

cd "${ROOT_DIR}"
cargo build --release --locked --bin image_viewer

rm -rf "${STAGE_DIR}"
mkdir -p "${STAGE_DIR}/bin" "${STAGE_DIR}/config"
install -m 0755 "${ROOT_DIR}/target/release/image_viewer" "${STAGE_DIR}/bin/image_viewer"
install -m 0644 "${ROOT_DIR}/config/viewer.example.yaml" "${STAGE_DIR}/config/viewer.example.yaml"
install -m 0644 "${ROOT_DIR}/README.md" "${STAGE_DIR}/README.md"

tar -C "${DIST_DIR}" -czf "${ARCHIVE}" "${PACKAGE_NAME}"
printf 'Packaged %s\n' "${ARCHIVE}"
