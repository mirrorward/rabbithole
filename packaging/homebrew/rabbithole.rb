# Homebrew formula for RabbitHole.
#
# Lives here so it is versioned with the code it installs; a tap repository
# (mirrorward/homebrew-tap) carries a copy that `brew install` reads. The
# release workflow bumps the copy — the url/sha256 below are exactly the
# tarballs and .sha256 files `release.yml` already publishes, so there is no
# second artifact to build for Homebrew's sake.
#
# Installing from this file directly:
#   brew install --formula packaging/homebrew/rabbithole.rb
class Rabbithole < Formula
  desc "Community server and clients in the lineage of BBSs and Hotline"
  homepage "https://rabbit.direct"
  version "0.191.0"
  license "AGPL-3.0-or-later"

  # Prebuilt archives per platform. Homebrew picks the matching one; building
  # from source would mean a full Rust toolchain and a ~10 minute install for
  # something the release already cross-compiles.
  on_macos do
    on_arm do
      url "https://github.com/mirrorward/rabbithole/releases/download/v#{version}/rabbithole-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 :no_check # replaced by the release workflow
    end
    on_intel do
      url "https://github.com/mirrorward/rabbithole/releases/download/v#{version}/rabbithole-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 :no_check
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/mirrorward/rabbithole/releases/download/v#{version}/rabbithole-v#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 :no_check
    end
  end

  def install
    # The release archive stages binaries beside a README in one directory.
    bin.install "burrow", "rabbit", "rabbit-tui", "looking-glass"
    doc.install "README.md" if File.exist?("README.md")
  end

  # A burrow holds accounts, boards, files and an identity key, so its data
  # directory is the one thing that must survive an upgrade or uninstall.
  # Homebrew keeps var/ across both.
  def post_install
    (var/"rabbithole").mkpath
  end

  service do
    run [opt_bin/"burrow", "--data-dir", var/"rabbithole", "run"]
    keep_alive true
    working_dir var/"rabbithole"
    log_path var/"log/rabbithole/burrow.log"
    error_log_path var/"log/rabbithole/burrow.error.log"
  end

  test do
    # Both halves of the install answer, and the version matches the formula —
    # a test that only checks the binary exists would pass on a stale archive.
    assert_match version.to_s, shell_output("#{bin}/burrow --version")
    assert_match version.to_s, shell_output("#{bin}/rabbit --version")
    assert_match "looking-glass", shell_output("#{bin}/looking-glass --help")
  end
end
