class Termgram < Formula
  desc "Focused, keyboard-first Telegram client for the terminal"
  homepage "https://github.com/iebb/termgram"
  version "0.1.40"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.40/termgram-0.1.40-macos.tar.gz"
      sha256 "f296b5ee267a29c3a3d4feec635d8b9bcdf793ff762b28ee42e9cc381389129d"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.40/termgram-0.1.40-macos-x86_64.tar.gz"
      sha256 "95f26153f487522fbe179ef56ae13b6ce109c1c168c9771f10fa29746263425b"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.40/termgram-0.1.40-linux-aarch64.tar.gz"
      sha256 "66ebb4138964f84d64143113cca99a33259c96f627fbc9af6591f63935bf22db"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.40/termgram-0.1.40-linux.tar.gz"
      sha256 "d2d0eaed235c567593e7187d28fc67ff6b8d416ed8e3c73c3c78ad0d7685edd6"
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
