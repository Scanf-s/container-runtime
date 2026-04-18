#!/bin/bash

# set -e: Exit immediately if any command returns non-zero (error)
# set -u: exit if an unset variable is referenced
# set -o pipefail: if any command in pipeline fails, the whole pipeline fails.
set -euo pipefail

# Use the first CLI argument if provided, else default to ./rootfs
ROOTFS_DIR="${1:-./rootfs}"
VERSION="3.20.3"
ARCH="$(uname -m)"
URL="https://dl-cdn.alpinelinux.org/alpine/v${VERSION%.*}/releases/${ARCH}/alpine-minirootfs-${VERSION}-${ARCH}.tar.gz"

# If the rootfs already exists and has contents, skip the following instructions
if [ -d "$ROOTFS_DIR" ] && [ -n "$(ls -A "$ROOTFS_DIR" 2>/dev/null)" ]; then
  echo "rootfs exists at $ROOTFS_DIR -> skipping"
  exit 0
fi

mkdir -p "$ROOTFS_DIR"
echo "fetching $URL"

# --no-same-owner: Forces extracted files to be owned by the user running the command (host: me, container: root)
# --no-same-permissions: Disregard permissions from the archive, applying the user's umask to the new files
# umask: Default file/directory permission masking value (default value: 022)
curl -fsSL "$URL" | tar -xz --no-same-owner --no-same-permissions -C "$ROOTFS_DIR"
echo "extracted alpine-minirootfs to $ROOTFS_DIR"
