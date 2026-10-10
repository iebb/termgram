class TermgramPre < Formula
  desc "Focused, keyboard-first Telegram client for the terminal"
  homepage "https://github.com/iebb/termgram"
  version "0.1.47"
  license "MIT"

  conflicts_with "termgram", because: "both install the tg binary"

  on_macos do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.47/termgram-0.1.47-macos.tar.gz"
      sha256 "2a14a06f49b303d80fe92cc1a59c3356b5c6d35c9d28593b730c578fa70e4869"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.47/termgram-0.1.47-macos-x86_64.tar.gz"
      sha256 "bcf492dcfed8364482c164d0b6db9d9f3df6ed55b97a752d7dad9a58353f9a7b"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.47/termgram-0.1.47-linux-aarch64.tar.gz"
      sha256 "0827ddb7403e94c783f83b05256f5bccd9dc7298d2ea99f7d965725de904f80e"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.47/termgram-0.1.47-linux.tar.gz"
      sha256 "70dcbe88a4edc72998fdd24ad0bae28b2369f235c2ec0cdf753dea51c67beece"
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
