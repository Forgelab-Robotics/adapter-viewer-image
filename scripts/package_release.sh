#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="${ROOT_DIR}/dist"
ARTIFACT="${DIST_DIR}/image_viewer"
TARGET="x86_64-unknown-linux-gnu"
BUILT_ARTIFACT="${ROOT_DIR}/target/${TARGET}/release/image_viewer"
HOME_DIR="${HOME:?HOME must be set}"
CARGO_HOME_DIR="${CARGO_HOME:-${HOME_DIR}/.cargo}"

cd "${ROOT_DIR}"
export LC_ALL=C
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-0}"
export CARGO_TARGET_DIR="${ROOT_DIR}/target"
unset CARGO_BUILD_TARGET CARGO_ENCODED_RUSTFLAGS
export RUSTFLAGS="--remap-path-prefix=${CARGO_HOME_DIR}=/cargo --remap-path-prefix=${ROOT_DIR}=. --remap-path-prefix=${HOME_DIR}=/build"

rm -rf "${DIST_DIR}"
trap 'rm -rf "${DIST_DIR}"' ERR

cargo build --release --locked --target "${TARGET}" --bin image_viewer
file "${BUILT_ARTIFACT}" | grep -Eq 'ELF 64-bit LSB.*x86-64'
readelf -h "${BUILT_ARTIFACT}" | grep -q 'Machine:.*Advanced Micro Devices X86-64'

mkdir -p "${DIST_DIR}"
install -m 0755 "${BUILT_ARTIFACT}" "${ARTIFACT}"
trap - ERR
printf 'Packaged %s\n' "${ARTIFACT}"
