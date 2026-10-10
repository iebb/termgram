cask "termgram@pre" do
  arch arm: "macos", intel: "macos-x86_64"

  version "0.1.47"
  sha256 arm:   "2a14a06f49b303d80fe92cc1a59c3356b5c6d35c9d28593b730c578fa70e4869",
         intel: "bcf492dcfed8364482c164d0b6db9d9f3df6ed55b97a752d7dad9a58353f9a7b"

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
