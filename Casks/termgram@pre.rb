cask "termgram@pre" do
  arch arm: "macos", intel: "macos-x86_64"

  version "0.1.39"
  sha256 arm:   "b17a7f818a37d31b894d61291e0382ff311bb4fdf6df85826310b19d9f86f15d",
         intel: "eb53f368cc73580dfe69eb14d01a7e173669c929a6beda8a9757abf6174ceb94"

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
