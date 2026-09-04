#!/bin/sh
# Render the Homebrew formula for a published release.
#   scripts/brew-formula.sh v0.1.0 > Formula/exa-pool.rb
# Reads the .sha256 files attached to the release; needs curl only.
set -eu

tag=${1:?usage: brew-formula.sh vX.Y.Z}
version=${tag#v}
repo="https://github.com/trancong12102/exa-pool"
base="$repo/releases/download/$tag"

# upload-rust-binary-action names the checksum exa-search-<target>.sha256 and
# writes "<hex>  <archive>". Refuse to render anything but a 64-hex digest.
sha() {
  digest=$(curl -fsSL "$base/exa-search-$1.sha256" | cut -d' ' -f1) || digest=""
  case "$digest" in
    *[!0-9a-f]* | "") echo "brew-formula: no sha256 for $1 at $tag" >&2; exit 1 ;;
  esac
  [ ${#digest} -eq 64 ] || { echo "brew-formula: bad sha256 for $1" >&2; exit 1; }
  printf '%s' "$digest"
}

mac_arm=$(sha aarch64-apple-darwin) || exit 1
mac_intel=$(sha x86_64-apple-darwin) || exit 1
linux_arm=$(sha aarch64-unknown-linux-musl) || exit 1
linux_intel=$(sha x86_64-unknown-linux-musl) || exit 1

cat <<RUBY
class ExaPool < Formula
  desc "Exa API CLI backed by a persistent round-robin pool of API keys"
  homepage "$repo"
  version "$version"
  license "MIT"

  on_macos do
    on_arm do
      url "$base/exa-search-aarch64-apple-darwin.tar.gz"
      sha256 "$mac_arm"
    end
    on_intel do
      url "$base/exa-search-x86_64-apple-darwin.tar.gz"
      sha256 "$mac_intel"
    end
  end

  on_linux do
    on_arm do
      url "$base/exa-search-aarch64-unknown-linux-musl.tar.gz"
      sha256 "$linux_arm"
    end
    on_intel do
      url "$base/exa-search-x86_64-unknown-linux-musl.tar.gz"
      sha256 "$linux_intel"
    end
  end

  def install
    bin.install "exa-search"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/exa-search --version")
  end
end
RUBY
