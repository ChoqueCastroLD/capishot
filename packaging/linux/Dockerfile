# The Linux release build environment: Ubuntu 22.04 (glibc 2.35), so the binaries run on
# Ubuntu 22.04, Debian 12, Mint 21 and anything newer. Used by packaging/linux/package.sh.
FROM ubuntu:22.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl git build-essential pkg-config python3 file \
      libxkbcommon-dev libwayland-dev libx11-dev libxrandr-dev libxi-dev libgl1-mesa-dev libgtk-3-dev \
    && rm -rf /var/lib/apt/lists/*
# Rust lives in the mounted CARGO_HOME/RUSTUP_HOME volumes (the toolchain is installed on first use).
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
