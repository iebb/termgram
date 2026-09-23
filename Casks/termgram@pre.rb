cask "termgram@pre" do
  arch arm: "macos", intel: "macos-x86_64"

  version "0.1.32"
  sha256 arm:   "03662365781238621f1abda9ae45a368688bb38a424f19dda2258352dd23c8af",
         intel: "13ceb8da54ccd3a0d3838929c246afaf43d37e9c72d6c5021ca76d2e87d6e67e"

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
