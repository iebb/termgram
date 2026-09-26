class TermgramPre < Formula
  desc "Focused, keyboard-first Telegram client for the terminal"
  homepage "https://github.com/iebb/termgram"
  version "0.1.39"
  license "MIT"

  conflicts_with "termgram", because: "both install the tg binary"

  on_macos do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.39/termgram-0.1.39-macos.tar.gz"
      sha256 "b17a7f818a37d31b894d61291e0382ff311bb4fdf6df85826310b19d9f86f15d"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.39/termgram-0.1.39-macos-x86_64.tar.gz"
      sha256 "eb53f368cc73580dfe69eb14d01a7e173669c929a6beda8a9757abf6174ceb94"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.39/termgram-0.1.39-linux-aarch64.tar.gz"
      sha256 "6d1ea8a5fd2c48583cebdf474340fa7c0a5e48528a7e70643f26298343c0ee1e"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.39/termgram-0.1.39-linux.tar.gz"
      sha256 "0193ffe4a89b28dfb7f35d9cbb06cfc5b9303eb3f6100d13191d65859c2e5ba3"
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
