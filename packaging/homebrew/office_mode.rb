class OfficeMode < Formula
  desc "Pixel-art terminal office for your AI coding agents"
  homepage "https://github.com/Rossonerian/office_mode"
  version "1.0.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Rossonerian/office_mode/releases/download/v#{version}/office_mode-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER_AARCH64_APPLE_DARWIN"
    end

    on_intel do
      url "https://github.com/Rossonerian/office_mode/releases/download/v#{version}/office_mode-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER_X86_64_APPLE_DARWIN"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Rossonerian/office_mode/releases/download/v#{version}/office_mode-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "PLACEHOLDER_AARCH64_UNKNOWN_LINUX_GNU"
    end

    on_intel do
      url "https://github.com/Rossonerian/office_mode/releases/download/v#{version}/office_mode-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "PLACEHOLDER_X86_64_UNKNOWN_LINUX_GNU"
    end
  end

  def install
    bin.install "office"
    bin.install "office-hook"
    bin.install_symlink "office" => "office_mode"
    (share/"office_mode").install "assets"
  end

  def caveats
    <<~EOS
      office_mode installs `office`, `office_mode`, and `office-hook`.
      To wire Claude Code and Antigravity hooks automatically, run:
        office install-hooks
        office doctor
    EOS
  end

  test do
    assert_match "office #{version}", shell_output("#{bin}/office --version")
    assert_match "office #{version}", shell_output("#{bin}/office_mode --version")
    assert_match "themes", shell_output("#{bin}/office doctor")
  end
end
