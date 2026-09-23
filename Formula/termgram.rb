class Termgram < Formula
  desc "Focused, keyboard-first Telegram client for the terminal"
  homepage "https://github.com/iebb/termgram"
  version "0.1.34"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.34/termgram-0.1.34-macos.tar.gz"
      sha256 "abf65a33f98d37000c73fe5d403c1b518ec8ee852ce3b1cfde113a29ef45be69"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.34/termgram-0.1.34-macos-x86_64.tar.gz"
      sha256 "5eda3862eb54a1514078d3d6214e4ac64278ed626c29d6f3fcaa3832fad4779b"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.34/termgram-0.1.34-linux-aarch64.tar.gz"
      sha256 "5ad72eb8bce543129456869c5372a368d3d6598a1639a7de6efc08802a516a8a"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.34/termgram-0.1.34-linux.tar.gz"
      sha256 "d879cc9fcbfe2d02daf66359b96ea7f3fc7f18dcb32da804117580c5db3596c9"
    end
  end

  def install
    bin.install "tg"
  end

  def caveats
    <<~EOS
      Run `tg` to launch Termgram.
      Update this installation with `brew upgrade termgram` instead of `tg update`
      so Homebrew can track the installed version.
    EOS
  end

  test do
    assert_match(/^(?:tg|version)\s+#{Regexp.escape(version.to_s)}$/, shell_output("#{bin}/tg --version"))
    assert_match "Open the TUI", shell_output("#{bin}/tg --help")
    assert_match "unknown argument", shell_output("#{bin}/tg invalid-command 2>&1", 1)
  end
end
