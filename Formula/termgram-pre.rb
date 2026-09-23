class TermgramPre < Formula
  desc "Focused, keyboard-first Telegram client for the terminal"
  homepage "https://github.com/iebb/termgram"
  version "0.1.27"
  license "MIT"

  conflicts_with "termgram", because: "both install the tg binary"

  on_macos do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.27/termgram-0.1.27-macos.tar.gz"
      sha256 "571de4f86f70ae94331fea5bc263c7b582ec7be96d7061674f7f953d9e824ff1"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.27/termgram-0.1.27-macos-x86_64.tar.gz"
      sha256 "fc272e0a4d0ef08d776b6e19d2d3f5d741da3411040a5905242c225f9c35b929"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.27/termgram-0.1.27-linux-aarch64.tar.gz"
      sha256 "6e8780ccca7553e4eae87c541c8a05172fa797e30c4164b0aaf9e34969572085"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.27/termgram-0.1.27-linux.tar.gz"
      sha256 "a94cee176930fb5b079e5404d47cbee466a205d995c211d38055afede759d3d3"
    end
  end

  def install
    bin.install "tg"
  end

  def caveats
    <<~EOS
      termgram-pre tracks the latest published prerelease.
      Run `tg` to launch Termgram.
      Update this installation with `brew upgrade termgram-pre` instead of `tg update`.
    EOS
  end

  test do
    assert_match(/^(?:tg|version)\s+#{Regexp.escape(version.to_s)}$/, shell_output("#{bin}/tg --version"))
    assert_match "Open the TUI", shell_output("#{bin}/tg --help")
    assert_match "unknown argument", shell_output("#{bin}/tg invalid-command 2>&1", 1)
  end
end
