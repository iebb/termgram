cask "termgram@pre" do
  arch arm: "macos", intel: "macos-x86_64"

  version "0.1.27"
  sha256 arm:   "571de4f86f70ae94331fea5bc263c7b582ec7be96d7061674f7f953d9e824ff1",
         intel: "fc272e0a4d0ef08d776b6e19d2d3f5d741da3411040a5905242c225f9c35b929"

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
