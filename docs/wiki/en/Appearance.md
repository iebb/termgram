# Appearance

[简体中文](../zh-CN/Appearance.md) · [Guide](Home.md)

Message headers show the full display name and `@username` when Telegram supplies
a public handle. They wrap on narrow terminals instead of truncating either name.
The handle is cached with the message; older cached records gain it when their
history is refreshed. Accounts without a public username keep their display name.

Press `c` on a chat, or in its conversation, to choose its color. Press `C` in the
chat list to color the current folder. Up/Down selects a named terminal color;
Enter applies it and Esc cancels. The list marker and selection styling remain
visible independently of the chosen color.

Chat rows and conversation titles use the terminal's default foreground unless
configured otherwise. Folder titles use a stable color chosen from six ANSI
palette entries. Your terminal theme controls the actual RGB values; the app
does not assume a dark background for these colors.

Precedence is built-in default → Lua → in-app override. Choose **Follow
configuration** to remove an override. **Terminal default** is an explicit color
choice and can override a colored Lua default.

```lua
return {
  colors = {
    chats = { [-1001234567890] = "cyan" },
    folders = { [2] = "yellow" },
  },
}
```

Values are `default`, `black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`,
`gray`, `dark_gray`, `light_red`, `light_green`, `light_yellow`, `light_blue`,
`light_magenta`, `light_cyan`, and `white`. `g i` shows chat and current folder IDs.

In-app overrides live in `appearance.json` beside `settings.conf`, keyed by the
actual Telegram account ID. Switching accounts or clearing the message cache
does not mix or delete them. Writes reuse the atomic settings writer. A malformed
preferences file is reported and is not silently replaced. Your Lua configuration
and the server's folder colors are never rewritten by this picker.

## Nerd Font icons

Nerd Font support is opt-in. Install a [Nerd Font](https://www.nerdfonts.com/font-downloads)
and select its **Nerd Font Mono** variant in the terminal profile, then add this
to `config.lua` and restart Termgram:

```lua
return {
  nerd_font = true,
}
```

Use a v3+ font. Icons identify chat types, folders, Archive, pins and attachments;
text labels, key hints and terminal colors remain visible. The Mono variant keeps
icons within terminal cells. With SSH or tmux, configure the font on the terminal
that displays the session. Termgram uses the terminal's selected font and does
not install fonts or change terminal preferences.

The default `nerd_font = false` retains the ordinary text presentation and `^`
pin marker. Turn the option off if glyphs appear as boxes or overlap adjacent text.
Configuration errors are reported through the existing Lua configuration loader.

Font reference: [Nerd Fonts font variants](https://github.com/ryanoasis/nerd-fonts/wiki/FAQ-and-Troubleshooting).

## Chat list columns

Titles use the configured chat color. Time defaults to cyan and unread counts to
yellow; both are right-aligned independently of title length. A marker and an
underlined selected title identify focus without covering colors with a reversed
row. Nerd Font chat/pin icons share the title column; CJK and emoji names truncate
by terminal cells. The compact sidebar defaults to 30 columns and F4 toggles it.
See [Lua configuration](Configuration.md) for width and semantic color options.

## Conversation layout

Each message has a separate author header. Full user names include both first and
last names, and long names wrap instead of being cut off by metadata. Incoming
authors use stable terminal colors; outgoing authors use green. Body text keeps
the terminal's default foreground.

Messages have no empty separator row by default. Alternating backgrounds use a
subtle shade of the terminal's reported background, working with dark and light
themes. Until a terminal reports its background, both use the default background.
Configure density or choose an ANSI background explicitly:

```lua
messages = {
  spacing = 0, -- 0–2 blank rows
  alternating = true,
  -- alternate_background = "dark_gray", -- omit for automatic theme shading
  -- images = "placeholder", -- show a label row; `o` opens the preview explicitly
},
```

The bottom bar's `message` item shows the selected message, or the last visible
message when no selection is active: local time/date, optional message ID,
delivery, pin and download/preview state. Select a message to inspect its details;
`context` supplies the effective Lua keys. On small terminals lower-priority
segments yield space and details can be shortened.

Replies show an indented author and original-message excerpt above text/media,
limited to two rows. Click the quote, or select the message and press Enter, to
jump to the original. IDs and loading/unavailable details belong to the bottom
bar. Clicking media still only selects it; `o` opens its preview. A thin left marker identifies the
selected message; the current reply, attachment or link action also gains emphasis.
Body, action rows and inline media share a small two-column gutter. Inline images
do not repeat a `photo` or `sticker` filename above the preview. While an image is
not available, a small placeholder preserves its target; preview status appears
in the bottom bar. Set `messages.images = "placeholder"` to skip inline rendering
entirely: image messages keep their label row and `o` still expands the preview on
demand, so no media downloads or image protocol output run while scrolling. `o` expands selected media, `O` reveals its original in the
system file manager, `i` replies, and `g n` / `g p` move among actions.

Resizing and sidebar toggles retain the anchored message. If reflow removes the
old physical row, the viewport returns to that message's header rather than its
trailing separator. This is a message/row anchor, not an exact text-character
position across different wrapping widths.
