class TermgramPre < Formula
  desc "Focused, keyboard-first Telegram client for the terminal"
  homepage "https://github.com/iebb/termgram"
  version "0.1.30"
  license "MIT"

  conflicts_with "termgram", because: "both install the tg binary"

  on_macos do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.30/termgram-0.1.30-macos.tar.gz"
      sha256 "604cb48f3db402deda630156d78979a874ae299608935b07fda2b4b169aa1c42"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.30/termgram-0.1.30-macos-x86_64.tar.gz"
      sha256 "6f394cb80595fad79a474dd96064d7bc7692f42ffa8c9172dc17bc34a02ec94c"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.30/termgram-0.1.30-linux-aarch64.tar.gz"
      sha256 "a279095ec6a7564464e70c0b959a739846d680077bd8f0af3024d2062fb4f718"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.30/termgram-0.1.30-linux.tar.gz"
      sha256 "191e511cd56780c9807797457adcef6361e9909bf4409ea067a6d22c53d6237d"
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
