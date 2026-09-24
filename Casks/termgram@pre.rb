cask "termgram@pre" do
  arch arm: "macos", intel: "macos-x86_64"

  version "0.1.36"
  sha256 arm:   "0c5b5105e751e2d856b18e5fb7e77c881d380defd8806ca7b0daa3660e8014f3",
         intel: "90b1cc1f69776e9bf0740e0ef8cf3aa5ca1b1a47ffbc5bba2687bf3b669d4ea2"

  url "https://github.com/iebb/termgram/releases/download/v#{version}/termgram-#{version}-#{arch}.tar.gz"
  name "Termgram"
  desc "Focused, keyboard-first Telegram client for the terminal (prerelease)"
  homepage "https://github.com/iebb/termgram"

  binary "tg"

  caveats <<~EOS
    termgram@pre installs the same tg executable as the termgram and
    termgram-pre formulae; unlink those before linking this cask.
  EOS
end
