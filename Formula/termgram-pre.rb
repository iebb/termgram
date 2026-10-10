class TermgramPre < Formula
  desc "Focused, keyboard-first Telegram client for the terminal"
  homepage "https://github.com/iebb/termgram"
  version "0.1.44"
  license "MIT"

  conflicts_with "termgram", because: "both install the tg binary"

  on_macos do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.44/termgram-0.1.44-macos.tar.gz"
      sha256 "be680a79aa791200fe6bea70153afd444bb0a911f2204d058925c2f6d4247cfb"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.44/termgram-0.1.44-macos-x86_64.tar.gz"
      sha256 "4fa7e6e48ff2214d04ec7076cb31ee7348bdf014bb70f772dfd9a097ee559982"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.44/termgram-0.1.44-linux-aarch64.tar.gz"
      sha256 "36b5b1a969912f6393f0318f5d79842a3d2101d08d496be23a32178ad7106349"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.44/termgram-0.1.44-linux.tar.gz"
      sha256 "24cf039a2ca74afae1567f7151a0d71875e70c18a31e6b94b57698fe339a2263"
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
