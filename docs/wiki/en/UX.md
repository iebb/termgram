# Daily workflows

[简体中文](../zh-CN/UX.md) · [Guide](Home.md)

## Read and navigate

The compact chat list and conversation share a wide window, with the composer
aligned below the conversation. F4 toggles the sidebar. Narrow windows show one
pane at a time; F4 or returning to Chats reveals the list and retains the draft. A focus border and selection marker show where keys will act;
Tab switches focus. Esc clears an explicit message selection before returning
from a conversation to the list.
Use `j/k` to select messages, arrows to scroll their rendered rows, and `G` to
return to the latest page. While you read earlier messages, new arrivals retain
your reading position and show a count instead of pulling the view down.

Chats with unread messages open at the saved incoming read boundary. A compact
`Unread messages` separator stays at that entry point while receipts advance.
Local pages appear provisionally; Telegram verifies the position before automatic
receipts start. Use `g u` or `:unread` to locate the current first unread message
again. Continue with `j`, Down or PageDown at the end of a page to load newer
history without skipping the intervening messages. The footer shows `continue`
and `latest` hints while more history remains. `G` explicitly returns to the
latest page. A failed position lookup keeps the cache visible; retry with `g u`
or choose `G` to leave that position.

`g r` / `:read` marks the entire target chat read. `g U` / `:mark-unread` sets
Telegram's unread reminder, shown as a colored dot when there is no actual
unread count. It does not change receipts already sent to other participants.
Reopening and displaying that chat clears the reminder; ordinary visible-message
receipts remain bounded. Commands retain the selected chat even if the list
reorders while the command line is open.

`[`/`]` in the list switch [folders](Folders.md). `/` filters titles in that
folder. A [configured chat alias](Configuration.md) opens a stable Telegram ID
from All chats. `g i` shows the selected chat and folder IDs for configuration.

Cached content appears first. The connection state and loading indicator describe
background reconciliation. Cached search and local preferences work without a
connection; sending and server operations need an authenticated connection.
The outgoing queue is bounded, and a failed send stays visible for retry.


The bottom statusline shows keyboard mode, account and connection information.
Selection changes its contextual hints; INSERT identifies the live composer.
Optional Ping/DC diagnostics and reading position are configured in
[Lua](Configuration.md). Long errors expand above the bar so their cause remains
readable without replacing the current mode.

Author, reply, body and media have separate visual rows. The current message
and its active action have distinct markers. Full author names wrap. Messages
have alternating backgrounds and no empty separator row by default; time, optional
IDs, delivery and media state are shown in the bottom bar.

## Read formatted messages

Telegram bold, italic, underline and strikethrough formatting is preserved,
including nested styles and Unicode emoji. Inline code is highlighted; code
blocks keep indentation and trailing spaces and show their language when given.
Quotes have an indented rail. Code and text wrap to the available terminal width.
Custom emoji keep Telegram's Unicode fallback; the terminal does not play their
animation. Literal Markdown in an ordinary message stays literal.

Spoilers start covered with visible placeholders. Click their message row, or
select the spoiler action with `g n` / `g p` and press Enter, to reveal or hide
them. `:spoiler` does the same for the selected message. Telegram's collapsible
quotes start with at most three rows; click the quote or press Enter on its action
to expand/collapse it, or use `:quote`. The bottom bar shows the current action.
Reply navigation retains its own action and click area.

Reveals are local to this account and process, and reset when the text or its
formatting changes. Reply excerpts, chat previews, search results, pinned-message
summaries, forwarding/deletion previews and notifications always cover spoilers.
A message's link rows remain hidden until its spoilers are revealed, including
hidden link targets. Explicit copy/edit still uses the full original text.
Formatting is cached with messages; older cached entries acquire it when fetched
again. Termgram reads server entities; creating formatted text in the composer
is not part of this revision. Lua can bind `spoilers` and `expand_quote` in the
`conversation` context.

## Compose and reply

Click the composer or press `i` to write. From the chat list, `i` continues the
current conversation, or restores the last chat for this Telegram account after
restart. If it is unavailable, the highlighted chat is used; an empty list shows
a synchronization hint. The composer shows a configurable ghost hint when empty,
without a second permanent row repeating the send key. Enter sends; Shift-Enter
or Ctrl-J inserts a newline. Bot commands are typed here, including their `/`;
typing `/` at the start of a draft or `@` after whitespace opens a suggestion
popup listing the chat's bot commands and members. Tab and the arrow keys cycle
candidates, Enter accepts the highlighted row, and Esc dismisses the popup.
Ctrl-T opens the [sticker panel](Stickers.md) for Recent, Favorites and
installed sets; sending a sticker keeps the draft text and honors the reply.
Each chat has its own local draft, including its text, reply target and cursor.
Esc, account switches and normal restarts preserve it. Drafts are keyed by the
Telegram user identity, so reusing a local account slot does not expose another
account's draft. Background saves coalesce edits over 400 ms; normal exit flushes
the final edit. A forced kill or power loss can lose edits not yet committed.
These drafts are local to this installation; cloud draft synchronization is not
part of this revision.

Select a message and press `i` or `R` to reply, or use `R` with no selection for the
latest message. Clicking the composer preserves an existing reply draft without
creating a reply from the message selection. The composer shows the target. Esc cancels the reply first,
keeping text; another Esc returns to navigation. `r` opens a selected message's reply target. Clicking the indented quote also
jumps there; Enter opens the selected reply action. Excerpts load from memory,
then local cache, then a bounded background batch for visible missing targets.
An unavailable original is distinct from an excerpt still loading. Edits and
deletions update excerpts, including while a slower request is in flight. Failed sends keep their content and reply target; activating a
failed outgoing message returns it to the composer for retry.

## Edit a message or caption

Select a delivered message and press `e` or use `:edit`. Termgram fetches the
current original and checks its author, media type and Telegram's server-provided
editing time limit. Unsupported, forwarded or inline-bot messages show the
reason in the footer. The server remains authoritative about chat permissions.

The editor identifies the chat and message, and the footer shows EDIT plus the
effective keys. Enter saves; Shift-Enter or Ctrl-J adds a newline. Esc/Ctrl-C
closes the editor while keeping the edit. `e` or `:edit` resumes the saved edit
when the selection is on its message; on another message it edits that message
instead, replacing an untouched saved edit, while a modified one is kept and
reported. Ctrl-D in the editor or `:edit-discard` discards it. One edit is kept
per chat alongside its normal draft, reply and files. It survives account
switches and normal restarts, using the same local draft save lifecycle.

Failed saves keep the input. A changed server revision is reported as a conflict;
it never silently replaces your edit with new server text. To start from that
new original, discard the local edit and reopen it. Telegram does not provide an
atomic compare-and-set edit, so an external edit in the brief interval between
verification and save can still race. Saving an empty caption removes the caption;
a text message must remain nonempty. Media replacement is not part of this editor.

Formatting before/after the changed range is retained with UTF-16 offsets;
formatting spanning the full replacement adjusts its length. Partly changed
entities and changed hidden-link/emoji targets are removed instead of attached
to unrelated text. Editing several separated ranges may remove formatting
between the first and last change. Server edit updates follow the existing ordered
synchronization stream and update cached replies. The selected message's edit
time appears in the bottom bar.

## Delete a message

Select a delivered message, then `d` or `:delete`. A fresh server snapshot shows
the chat, message preview and available scopes. Cancel is initially selected;
choose a scope with Up/Down or k/j and press Enter to submit. Ordinary chats may
offer only-for-me and, when permitted, for-everyone deletion. Supergroups use
Telegram's for-everyone deletion; Saved Messages offers only-for-me. Service
messages are not supported by this action. The server enforces final permissions.

Termgram checks the exact peer, message revision and permissions again before
submitting. A changed message invalidates the review; close and reopen it. Telegram
does not offer an atomic revision-checked delete, so a remote change between the
last check and the RPC can still race. Errors keep the message and reset the
prompt to Cancel. Esc closes the prompt; if already submitted, the request keeps
running and reports its outcome in the footer. Confirmed deletion passes through
the ordered sync stream and removes cached content, search results and reply
previews before the corresponding sync checkpoint is stored.

## Copy message content

Select a delivered message and use `y` or `:copy` / `:copy text` for its text or
caption, without sender labels or timestamps. `Y` / `:copy link` asks Telegram for
its message link, including a topic/thread when available. Links are available
for supergroups/channels, including members-only private links; ordinary private
chats and basic groups have no exportable message link.

Copy checks the latest message and chat permissions online. Protected text is
not copied; empty text and unavailable messages leave the clipboard unchanged.
The captured message remains the target while the request runs, even if you
navigate elsewhere. One preparation and one clipboard write may run at a time.
The footer reports errors and uses your configured copy key in selection hints.

Local text writes use the native clipboard in the background. Linux retains its
clipboard owner while Termgram runs; pasting after exit depends on the desktop's
clipboard manager. WSL falls back to Windows PowerShell when needed. SSH sends
Yazi's OSC 52 request to the attached terminal, and tmux also receives a terminal
copy request. Native failure likewise falls back to OSC 52. Terminal delivery
has no acknowledgement: “Clipboard request sent” means the terminal still needs
to accept it, and tmux/terminal clipboard settings can prevent it. Copy payloads
are limited to 100,000 UTF-8 bytes and never written to logs or command arguments.

## Forward and Saved Messages

Select one delivered message and press `f`, or enter `:forward <chat>`. Tab uses
the same cached chat names, IDs and Lua aliases as `:chat`; `saved` is the current
account’s Saved Messages. Enter on an incomplete command chooses no destination.
After choosing a destination, a preview shows its name/ID, account, source and
latest message. Press Enter to forward, or Esc/Ctrl-C to cancel. These are the
`send`/`cancel` actions in the configurable `forward` context.

`g S` / `:save` prepares the same preview addressed to Saved Messages. `g s` /
`:saved` opens that conversation without forwarding anything. It is identified
by the authenticated account, so it works without a matching recent-chat title.
An already cached Saved Messages dialog can be opened offline.

Native forwarding keeps the original attribution and media and leaves existing
source/destination drafts untouched. Current protection is checked before the
preview and again before sending. Service messages, expiring media, protected
content or server restrictions produce an error. Remote edits/deletions invalidate
an open preview. The server remains authoritative about destination permissions;
an edit between the final check and submission can still race.

Errors keep the review and the same Telegram deduplication ID for retry. Esc
before submission discards the review; after submission it closes the view while
the request completes. If that background request fails, `f` resumes its review.
Reviews and their retry IDs currently last for this process; cancelling a review
and starting another creates a new forwarding intent. This entry point forwards
one selected message, not a whole album or multiple selections. The destination
picker selects chats; it does not select a specific forum topic.

## Files, links and mouse input

Use `:attach <paths...>` to prepare files in the chat draft. Plain pasted paths
remain text by default. Ctrl-O in the composer opens attachment review for format,
preview and removal; Enter in the composer explicitly sends, using its text and
reply target for the first file. See [Attachments](Attachments.md).

Click media to select it, then `o` for a larger preview or `i` to reply. Use
`g n` / `g p` to select actionable entries, then Enter to activate. Telegram
URL entities appear as selectable link rows, including hidden-text links.
Public `t.me`, `telegram.me`, and `tg://resolve` chat/message links open in-app;
private `t.me/c` and `tg://privatepost` links work for known groups. Other HTTP(S)
links open through the operating system. Invite links show a [confirmation preview](Chats.md); broadcast-channel links
remain unsupported. URL, web-view, callback and game bot buttons are supported;
payment, password-gated, contact/location and peer-selection buttons identify
that a graphical client is required.

Mouse support is optional: click a chat to open it, click a message or action to
select it, right-click a message to reply, and scroll the pane under the pointer.
Clicking the chat list or timeline leaves input mode and keeps the draft. Settings/account rows are also
clickable. Keyboard equivalents remain available.

## Settings and accounts

`s` opens automatic update checks, release channel, Enter download behavior,
and message IDs in the bottom bar. `c` and `C` open [color pickers](Appearance.md).
These persist without rewriting Lua. Esc dismisses an overlay, returning focus
to the previous view. `?` shows effective bindings instead of a static cheat sheet.

`a` opens the account picker; select an account or its add row and press Enter.
F2 cycles existing slots and F3 adds a slot, up to eight. Only the active account
has a running network worker. Sessions, messages, media, and color overrides are
isolated. Switching resets searches and conversation views while retaining each
account's local drafts.

To open a new private chat, use `:open @username`, then `i` to compose.
Use `:info` to inspect the chat's identity and write restrictions. See
[Chat discovery and details](Chats.md) for invitations and permissions.
