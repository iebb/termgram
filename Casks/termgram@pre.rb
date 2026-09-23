cask "termgram@pre" do
  arch arm: "macos", intel: "macos-x86_64"

  version "0.1.30"
  sha256 arm:   "604cb48f3db402deda630156d78979a874ae299608935b07fda2b4b169aa1c42",
         intel: "6f394cb80595fad79a474dd96064d7bc7692f42ffa8c9172dc17bc34a02ec94c"

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
