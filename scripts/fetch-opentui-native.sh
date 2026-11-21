#!/usr/bin/env bash
set -euo pipefail

VERSION="0.9.7"
OS="$(uname -s)"
ARCH="$(uname -m)"

case "${OS}-${ARCH}" in
  Darwin-arm64) PLATFORM="darwin-arm64" ;;
  Darwin-x86_64) PLATFORM="darwin-x64" ;;
  Linux-x86_64) PLATFORM="linux-x64" ;;
  Linux-aarch64|Linux-arm64) PLATFORM="linux-arm64" ;;
  *)
    echo "Unsupported platform: ${OS}-${ARCH}" >&2
    exit 1
    ;;
esac

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST_DIR="${ROOT_DIR}/vendor/opentui-native"
ARCHIVE="opentui-native-v${VERSION}-${PLATFORM}.zip"
URL="https://github.com/anomalyco/opentui/releases/download/v${VERSION}/${ARCHIVE}"

if [[ -f "${DEST_DIR}/opentui.h" && -f "${DEST_DIR}/libopentui.a" ]]; then
  echo "OpenTUI native library already present in ${DEST_DIR}"
  exit 0
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

echo "Downloading ${URL}..."
curl -fsSL -o "${TMP_DIR}/${ARCHIVE}" "${URL}"

mkdir -p "${DEST_DIR}"
unzip -oq "${TMP_DIR}/${ARCHIVE}" -d "${DEST_DIR}"

echo "Extracted OpenTUI native library to ${DEST_DIR}"
