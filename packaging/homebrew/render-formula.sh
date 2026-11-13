#!/usr/bin/env bash
# Print the Homebrew formula for a release.
#
#   render-formula.sh VERSION SHA256_ARM64 SHA256_X86_64
#
# The tarballs are the ones the release workflow attaches to the GitHub
# release v<VERSION>: ymacs-<VERSION>-{aarch64,x86_64}-apple-darwin.tar.gz.
set -euo pipefail

if [ $# -ne 3 ]; then
  echo "usage: $0 VERSION SHA256_ARM64 SHA256_X86_64" >&2
  exit 2
fi
version=$1 sha_arm=$2 sha_intel=$3
repo=${YMACS_REPO:-rchindris/ymacs}
base="https://github.com/${repo}/releases/download/v${version}"

cat <<RUBY
class Ymacs < Formula
  desc "Fast, GPU-rendered editor with Emacs keys"
  homepage "https://github.com/${repo}"
  version "${version}"
  license "MIT"

  depends_on :macos

  on_arm do
    url "${base}/ymacs-${version}-aarch64-apple-darwin.tar.gz"
    sha256 "${sha_arm}"
  end

  on_intel do
    url "${base}/ymacs-${version}-x86_64-apple-darwin.tar.gz"
    sha256 "${sha_intel}"
  end

  def install
    bin.install "ymacs"
  end

  test do
    assert_match "\"keymaps\"", shell_output("#{bin}/ymacs --print-default-config")
  end
end
RUBY
