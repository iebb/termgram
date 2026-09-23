class TermgramATPre < Formula
  desc "Focused, keyboard-first Telegram client for the terminal"
  homepage "https://github.com/iebb/termgram"
  version "0.1.25"
  license "MIT"

  conflicts_with "termgram", because: "both install the tg binary"

  on_macos do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.25/termgram-0.1.25-macos.tar.gz"
      sha256 "cd44185331e70f9083b1f35c2a5530744f7c76292545372e73f9a348e4452598"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.25/termgram-0.1.25-macos-x86_64.tar.gz"
      sha256 "d5a54fdb1c8d7088aca252eee9ed47db76caeb5883fff2caf504d65a5eb1e77c"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/iebb/termgram/releases/download/v0.1.25/termgram-0.1.25-linux-aarch64.tar.gz"
      sha256 "c871f833414251dd47dec147ed563bf72bcc7cf3ef80c36db4aa1d69408a48d8"
    end
    on_intel do
      url "https://github.com/iebb/termgram/releases/download/v0.1.25/termgram-0.1.25-linux.tar.gz"
      sha256 "7ac761b32710c1766000a9c4c3a1ed02b0a9b29b2c18868b1c745fc4d7050a75"
    end
  end

  def install
    bin.install "tg"
  end

  def caveats
    <<~EOS
      termgram@pre tracks the latest published prerelease.
      Run `tg` to launch Termgram.
      Update this installation with `brew upgrade termgram@pre` instead of `tg update`.
    EOS
  end

  test do
    assert_match(/^(?:tg|version)\s+#{Regexp.escape(version.to_s)}$/, shell_output("#{bin}/tg --version"))
    assert_match "Open the TUI", shell_output("#{bin}/tg --help")
    assert_match "unknown argument", shell_output("#{bin}/tg invalid-command 2>&1", 1)
  end
end
