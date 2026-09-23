-- Apply changes in the app with :config reload (or :reload).
-- Save as config.lua beside settings.conf, or set TERMGRAM_CONFIG.
-- Settings are declarative: return a table. See docs/wiki/en/Configuration.md.
return {
  chats = {
    -- work = -1001234567890, -- stable Telegram chat ID
  },
  ghost_text = "{send} to send · {stickers} for stickers",
  nerd_font = false, -- true when the terminal uses a Nerd Font Mono (v3+)
  sidebar = { width = 30, time_color = "cyan", unread_color = "yellow" },
  messages = { spacing = 0, alternating = true }, -- automatic terminal background shading
  -- messages.images = "placeholder" renders a label instead of inline media; `o` still opens it
  statusline = {
    enabled = true,
    left = { "mode", "app", "account", "message", "context" },
    right = { "notifications", "connection", "latency", "dc", "position" },
  },
  keymap = {
    -- { context = "conversation", on = { "<C-u>" }, run = "message_up", count = 20 },
    -- { context = "conversation", on = { "g", "w" }, run = "jump work" },
    -- { context = "compose", on = { "<Enter>" }, run = "newline" },
    -- { context = "compose", on = { "<C-s>" }, run = "send" },
  },
}
