class OfficeMode < Formula
  desc "Pixel-art terminal office for your AI coding agents"
  homepage "https://github.com/Rossonerian/office_mode"
  version "1.2.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Rossonerian/office_mode/releases/download/v#{version}/office_mode-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "cdacdb885b765492bf62766a23764c540f599b54942032dde7b17f37f04e78e0"
    end

    on_intel do
      url "https://github.com/Rossonerian/office_mode/releases/download/v#{version}/office_mode-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "86735a4c7419721c9f0c5f52e723e3f9a07e152f191ef40632624f5836399840"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Rossonerian/office_mode/releases/download/v#{version}/office_mode-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "7357191868e94736f06eed21c9c1ad4e1e366b486a9ac00d1dfabfbb60702d43"
    end

    on_intel do
      url "https://github.com/Rossonerian/office_mode/releases/download/v#{version}/office_mode-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "4fe7df436cb211c743771ab29865736d30aaa0c18510f95bdd5c9c011810b7b5"
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

  def test do
    assert_match "office #{version}", shell_output("#{bin}/office --version")
    assert_match "office #{version}", shell_output("#{bin}/office_mode --version")
    assert_match version.to_s, shell_output("#{bin}/office-desktop --version") if (bin/"office-desktop").exist?
    assert_match "themes", shell_output("#{bin}/office doctor")
  end
end
