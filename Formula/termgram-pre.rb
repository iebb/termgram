class TermgramPre < Formula
  desc "Focused, keyboard-first Telegram client for the terminal"
  homepage "https://github.com/iebb/termgram"
  version "0.1.36"
  license "MIT"

  conflicts_with "termgram", because: "both install the tg binary"

  on_macos do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.36/termgram-0.1.36-macos.tar.gz"
      sha256 "0c5b5105e751e2d856b18e5fb7e77c881d380defd8806ca7b0daa3660e8014f3"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.36/termgram-0.1.36-macos-x86_64.tar.gz"
      sha256 "90b1cc1f69776e9bf0740e0ef8cf3aa5ca1b1a47ffbc5bba2687bf3b669d4ea2"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.36/termgram-0.1.36-linux-aarch64.tar.gz"
      sha256 "d7eadf18f9b8b388cb02fe3a55ba17c0e8b672b285dacd126141d2212918a9bd"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.36/termgram-0.1.36-linux.tar.gz"
      sha256 "e1c253e333343fc6a8b787600680be60f3bcaccbf6a0d68054433d935856d7c9"
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
