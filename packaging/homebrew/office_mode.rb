class OfficeMode < Formula
  desc "Pixel-art terminal office for your AI coding agents"
  homepage "https://github.com/Rossonerian/office_mode"
  version "1.2.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Rossonerian/office_mode/releases/download/v#{version}/office_mode-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "18fc3e9330bc704a9c5e29c74022bc796daf2b22a8935d281ebf7aa3bdfb4648"
    end

    on_intel do
      url "https://github.com/Rossonerian/office_mode/releases/download/v#{version}/office_mode-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "16dccb46a68fe62fe6397864f55f6225adb8ee21478cfbfb515555ab0aa2e079"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Rossonerian/office_mode/releases/download/v#{version}/office_mode-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "2b6c57efa53af4596f3d790af8ecde3deed4aaf0a22b2a85b0fe2e3f71e9fbf8"
    end

    on_intel do
      url "https://github.com/Rossonerian/office_mode/releases/download/v#{version}/office_mode-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "ac934afecd242bb25d6d9bd165082d38c81c21484a44e321bf71e57849854866"
    end
  end

  def install
    bin.install "office"
    bin.install "office-hook"
    bin.install "office-desktop" if File.exist?("office-desktop")
    bin.install_symlink "office" => "office_mode"
    (share/"office_mode").install "assets"
  end

  def caveats
    <<~EOS
      office_mode installs `office`, `office_mode`, `office-hook`, and `office-desktop` (`office desktop`).
      To wire Claude Code and Antigravity hooks automatically, run:
        office install-hooks
        office doctor
    EOS
  end

  test do
    assert_match "office #{version}", shell_output("#{bin}/office --version")
    assert_match "office #{version}", shell_output("#{bin}/office_mode --version")
    assert_match version.to_s, shell_output("#{bin}/office-desktop --version") if (bin/"office-desktop").exist?
    assert_match "themes", shell_output("#{bin}/office doctor")
  end
end
