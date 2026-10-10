cask "termgram@pre" do
  arch arm: "macos", intel: "macos-x86_64"

  version "0.1.44"
  sha256 arm:   "be680a79aa791200fe6bea70153afd444bb0a911f2204d058925c2f6d4247cfb",
         intel: "4fa7e6e48ff2214d04ec7076cb31ee7348bdf014bb70f772dfd9a097ee559982"

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
