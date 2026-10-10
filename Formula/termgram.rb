class Termgram < Formula
  desc "Focused, keyboard-first Telegram client for the terminal"
  homepage "https://github.com/iebb/termgram"
  version "0.1.48"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.48/termgram-0.1.48-macos.tar.gz"
      sha256 "d5711bd67baa993be28f58ab8605456dce0021d0743d872306acb36b79fa560e"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.48/termgram-0.1.48-macos-x86_64.tar.gz"
      sha256 "24065258763bb6d6ae8e111f19a154a2b3e98739be64b85638da6aafa158a313"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.48/termgram-0.1.48-linux-aarch64.tar.gz"
      sha256 "8f06be50394611576022574c75b93f3293b7cebec61343913a533941ba0e7cfd"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.48/termgram-0.1.48-linux.tar.gz"
      sha256 "1dba263c9b6f807ee8c5eabd3465f1b11af4347522a22b6b6d405305c7f33dcc"
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
