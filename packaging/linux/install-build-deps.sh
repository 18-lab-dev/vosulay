#!/bin/sh
# Prepares the Ubuntu 22.04 package build environment (CI container and
# packaging/linux/Containerfile). Packages are built on the oldest supported
# glibc (2.35) so one binary runs on every target distribution.
set -eu
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends \
    build-essential pkg-config ca-certificates curl git python3 python3-tomli \
    libwayland-dev
if ! command -v rustup >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
        sh -s -- -y --profile minimal --default-toolchain none
fi
