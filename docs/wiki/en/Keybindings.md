# Keybindings

[简体中文](../zh-CN/Keybindings.md) · [Guide](Home.md)

These are defaults. `?` displays the current Lua bindings and available commands.
Press `/` or Ctrl-F to search keys, descriptions, contexts and commands. Matches
are highlighted as you type; search is literal and case-insensitive. Enter keeps
the filter while you scroll. Esc clears the search first, then closes help.
The `help` context configures browsing keys; `input` configures query editing.

 Uppercase keys mean
Shift plus that letter. `g g`, `g i`, `g n`, and `g p` are successive presses; a pending
chord times out after one second and Esc cancels it. Keys apply to the focused
pane or overlay, so ordinary letters in an editor remain text.

| Context | Keys | Action |
| --- | --- | --- |
| Chats / Conversation | g m | Browse unread mentions and replies to you |
| Search / Mentions | Ctrl-R | Refresh the current search or unread mentions |
| Global | Ctrl-C / Ctrl-L | Quit / redraw |
| Global | F2 / F3 | Next account / add account |
| Main view | F4 | Toggle sidebar; retain the draft |
| Login | Enter / Esc | Submit / restart phone sign-in |
| Login | Tab or Shift-Tab | Start QR login or change QR display |
| Chats | j/k or Down/Up | Select next/previous chat |
| Chats | Enter or Right | Open selected chat |
| Chats | i | Compose in the current chat, or resume the last chat for this account |
| Chats | G / g g | Last / first chat in the filtered list |
| Chats | PageDown/PageUp | Move ten chats |
| Chats | / | Filter titles in this folder |
| Chats | Ctrl-F | Open local regex search |
| Chats | [ / ] | Previous / next Telegram folder |
| Chats | c / C | Chat / folder color picker |
| Chats | p | Pin/unpin in the current folder |
| Chats | Ctrl-K / Ctrl-J | Move a pinned chat up/down |
| Chats | e | Archive / restore selected chat |
| Conversation | j/k or ]/[ | Select next/previous message |
| Conversation | 20k | Move up 20 messages; fetch older pages if needed |
| Conversation | Up/Down | Scroll rendered rows |
| Conversation | PageUp/PageDown | Scroll ten rendered rows |
| Conversation | G or End | Return to latest messages and follow incoming messages |
| Conversation | g u | Resume from the first unread message |
| Chats / Conversation | g r / g U | Mark the entire chat read / mark unread as a reminder |
| Conversation | g g or Home | Oldest message in the loaded window |
| Conversation | i | Reply to an explicitly selected message; otherwise compose |
| Conversation | Enter | Activate the selection; compose if nothing is selected |
| Conversation | R / r | Reply to selection/latest / open its reply target |
| Conversation | o | Expand selected image or sticker |
| Conversation | g n / g p | Next / previous actionable item |
| Conversation | l | Open the selected or first supported link |
| Conversation | O | Reveal attachment in the system file manager |
| Conversation | / / c | Local regex search / chat color |
| Conversation | p / P | Pin/unpin selected message / browse pins |
| Conversation | v | Open the selected [poll or quiz](Polls.md) |
| Poll | Space/Enter, then Ctrl-S | Choose answers, then submit |
| Poll | u / Ctrl-R / Esc | Prepare vote retraction / refresh / close |
| Conversation | g e | Open the selected message's [reactions](Reactions.md) |
| Reactions | Enter/Space / u / Ctrl-R / Esc | Toggle an emoji / remove your emoji reactions / refresh / close |
| Chat details (`:info`) | j/k or mouse wheel / Ctrl-R / Esc | Scroll / refresh / close |
| Invite preview (`:join`) | Up/Down, then Enter | Choose and confirm; initially selects Cancel |
| Help | / or Ctrl-F / Enter / Esc | Search and highlight / keep filter / clear search, then close |
| Help / Status | j/k or mouse wheel / Esc | Scroll wrapped content / close (help clears search first) |
| Pinned messages | Enter / Esc | Open selected message / close |
| Pinned messages | Ctrl-N / Ctrl-P | Next / previous page |
| Pinned messages | p / U | Confirm unpin / unpin all |
| Pinned messages | Ctrl-R | Refresh pins |
| Navigation | Tab or Shift-Tab | Switch between chat list and conversation |
| Conversation | Esc | Clear message selection first; then return to the chat list |
| Conversation | Left | Return to the chat list |
| Image preview | Esc or q / i / O | Close / reply / reveal original file |
| Navigation | g i / Ctrl-R | Show chat and folder IDs / refresh lists |
| Navigation | ? / s / a / q | Help / settings / accounts / quit |
| Navigation | : | Open the [command line](Commands.md) |
| Commands | Tab/Shift-Tab or Ctrl-N/Ctrl-P | Complete next/previous candidate |
| Commands | Up/Down | Prefix-matching command history |
| Commands | Enter / Esc or Ctrl-C | Execute / cancel |
| Composer | Enter | Send |
| Composer | Shift-Enter or Ctrl-J | Newline |
| Composer | Ctrl-T | Open the [sticker panel](Stickers.md) |
| Composer | Esc | Cancel reply first; then leave with draft kept |
| Sticker panel | hjkl or arrows / Tab or Shift-Tab | Move in the grid / switch section |
| Sticker panel | Enter or Space / click the selection | Send the selected sticker |
| Sticker panel | Ctrl-R / Esc or q | Refetch the lists / close |
| Editors | Left/Right, Home/End, Ctrl-A/Ctrl-E | Move cursor |
| Editors | Backspace/Delete, Ctrl-W, Ctrl-U | Delete character / previous word / clear |
| Chat filter | Enter / Esc | Open match / clear filter and leave |
| Overlays | Up/Down or k/j | Select or scroll |
| Overlays | Enter or Space | Apply setting, color, or account selection |
| Overlays | Esc or ? | Close |
| Search | Enter / Esc | Search or open result / close |
| Search | Tab / Ctrl-F | Change scope / edit query |
| Search | Ctrl-N / Ctrl-P | Next / previous result page |
| Search | Up/Down, PageUp/PageDown | Select result / move ten results |
| Conversation | e | Edit selected message or resume local edit |
| Conversation | d | Review deletion for the selected delivered message |
| Conversation | y / Y | Copy message text or caption / copy Telegram message link |
| Conversation | f / g S | Choose a forward destination / prepare a forward to Saved Messages |
| Navigation | g s | Open Saved Messages for the active account |
| Forward preview | Enter / Esc or Ctrl-C | Forward / cancel; a submitted request continues in the background |
| Deletion prompt | Up/Down or k/j, then Enter | Choose scope and confirm; initially selects Cancel |
| Deletion prompt | Esc | Close; an already submitted request continues |
| Message editor | Enter / Esc or Ctrl-C / Ctrl-D | Save / keep and close / discard edit |
| Message editor | Shift-Enter or Ctrl-J | Newline |

A number prefix such as `20k` works in the two navigation panes. For a single
key that jumps N messages, use `count` in [Lua configuration](Configuration.md).
`gg` is scoped to loaded history; it does not download every message back to the
start of a group. `G` reloads the latest page when reading older history.

Changed defaults from earlier Termgram versions: conversation `/` now opens
local search; type bot `/commands` after `i`. Uppercase `O` now reveals files;
action cycling moves to `g n` / `g p`, and `o` expands an image preview.
`i` also works from the chat list and replies when a message is explicitly selected. Use the account picker with arrows and Enter;
digit keys in navigation are counts, and the old account-number hints are gone.
There is no separate hardcoded keyboard fallback after Lua resolution.

Click a reply quote to jump to its original. For a selected reply, Enter opens
that target by default; use `g n` / `g p` for its other actions. Media clicks select
the attachment, so Enter then activates that media action.

Composer Ctrl-O opens attachment review. Its a/p/d/o/O/i keys add, choose photo or file, remove, preview, reveal and edit the caption. See [Attachments](Attachments.md). Composer Ctrl-T opens the [sticker panel](Stickers.md) for Recent, Favorites and installed sets.

Cmd-V / Ctrl-V / Alt-V / Ctrl-Alt-V paste native clipboard files, images or text in the conversation, composer and attachment review. `:paste` is the command equivalent; no paste sends automatically.
