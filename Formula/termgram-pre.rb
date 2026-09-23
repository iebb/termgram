class TermgramPre < Formula
  desc "Focused, keyboard-first Telegram client for the terminal"
  homepage "https://github.com/iebb/termgram"
  version "0.1.32"
  license "MIT"

  conflicts_with "termgram", because: "both install the tg binary"

  on_macos do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.32/termgram-0.1.32-macos.tar.gz"
      sha256 "03662365781238621f1abda9ae45a368688bb38a424f19dda2258352dd23c8af"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.32/termgram-0.1.32-macos-x86_64.tar.gz"
      sha256 "13ceb8da54ccd3a0d3838929c246afaf43d37e9c72d6c5021ca76d2e87d6e67e"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.32/termgram-0.1.32-linux-aarch64.tar.gz"
      sha256 "df00db1cafc4e184f2398ee108afcd71455a054a11be5c940c0c59b030396300"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.32/termgram-0.1.32-linux.tar.gz"
      sha256 "c88a4f646eb164ccfb672f36c461c5b180fbb026dd95001a16a1019576a1e826"
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
