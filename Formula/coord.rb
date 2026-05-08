# Homebrew formula for `coord`.
#
# Two ways to install once a release is tagged:
#
#   1. From this repo (build from source):
#        brew install --build-from-source --HEAD \
#          https://raw.githubusercontent.com/DmarshalTU/coord/main/Formula/coord.rb
#
#   2. Via the tap (binary downloads, recommended once releases ship):
#        brew tap dmarshaltu/coord
#        brew install coord
#
# To populate the binary URLs below, run `scripts/release-formula.sh vX.Y.Z`
# after a successful release build.
class Coord < Formula
  desc "Local coordinator for parallel AI coding agents (MCP + A2A, atomic claims)"
  homepage "https://github.com/DmarshalTU/coord"
  license "MIT"
  version "0.3.0"

  # Pre-built binaries: Apple Silicon macOS, x86_64 Linux, x86_64 Windows.
  # On Intel Macs, ARM Linux, and other architectures, install with:
  #   cargo install --git https://github.com/DmarshalTU/coord
  on_macos do
    on_arm do
      url "https://github.com/DmarshalTU/coord/releases/download/v#{version}/coord-aarch64-apple-darwin"
      sha256 "REPLACE_WITH_SHA256_AARCH64_DARWIN"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/DmarshalTU/coord/releases/download/v#{version}/coord-x86_64-unknown-linux-gnu"
      sha256 "REPLACE_WITH_SHA256_X86_64_LINUX"
    end
  end

  def install
    bin.install Dir["coord-*"].first => "coord"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/coord version")
  end
end
