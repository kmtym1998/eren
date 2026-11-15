# Homebrew formula template for eren.
#
# This file is the source for Formula/eren.rb in the separate tap repository
# (kmtym1998/homebrew-tap). On each release:
#   1. Bump `version`.
#   2. Replace each sha256 with the contents of the matching
#      `eren-<version>-<target>.tar.gz.sha256` file attached to the
#      GitHub Release.
#   3. Copy this file into the tap repo as Formula/eren.rb and commit.
#
# NOTE: the release tarballs must be downloadable without authentication,
# so kmtym1998/eren has to be a public repository for this formula to work
# for anyone but the owner.
class Eren < Formula
  desc "Native workspace for coding agents (Sirio/Herdr-like, built with GPUI)"
  homepage "https://github.com/kmtym1998/eren"
  version "0.1.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/kmtym1998/eren/releases/download/v#{version}/eren-#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_AARCH64_APPLE_DARWIN_SHA256"
    end

    on_intel do
      url "https://github.com/kmtym1998/eren/releases/download/v#{version}/eren-#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_X86_64_APPLE_DARWIN_SHA256"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/kmtym1998/eren/releases/download/v#{version}/eren-#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_WITH_X86_64_UNKNOWN_LINUX_GNU_SHA256"
    end
  end

  def install
    bin.install "eren"
  end

  def caveats
    <<~EOS
      eren is a GUI app; run `eren` from a terminal to open it.
      The binary is not notarized yet, so on first launch macOS may ask you
      to allow it in System Settings > Privacy & Security.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/eren --version")
  end
end
