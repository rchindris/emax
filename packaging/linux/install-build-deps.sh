#!/usr/bin/env bash
# System libraries GPUI needs to build on Debian/Ubuntu (X11, Wayland, Vulkan, fonts).
set -euo pipefail

sudo=""
[ "$(id -u)" -ne 0 ] && sudo="sudo"

$sudo apt-get update -qq
$sudo apt-get install -y -qq --no-install-recommends \
  pkg-config cmake clang \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
  libx11-xcb-dev libxcb1-dev libvulkan-dev \
  libfontconfig-dev libfreetype-dev \
  libasound2-dev libssl-dev libzstd-dev
