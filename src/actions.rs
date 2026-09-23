//! One typed action vocabulary for Lua, help and the application command line.
//!
//! Like Codex's slash commands and which-key descriptions, metadata belongs to
//! the action, not to each presentation of it. No upstream runtime is copied.

macro_rules! actions {
    ($( $variant:ident => ($name:literal, $description:literal) ),+ $(,)?) => {
        #[derive(Clone, Debug, Eq, PartialEq)]
        pub enum Action {
            $( $variant, )+
            Jump(String),
        }
        impl Action {
            #[must_use]
            pub fn parse(value: &str) -> Option<Self> {
                match value {
                    $( $name => Some(Self::$variant), )+
                    _ => value.strip_prefix("jump ")
                        .filter(|alias| !alias.is_empty())
                        .map(|alias| Self::Jump(alias.to_owned())),
                }
            }
            #[must_use]
            pub const fn name(&self) -> &'static str {
                match self { $( Self::$variant => $name, )+ Self::Jump(_) => "jump" }
            }
            #[must_use]
            pub const fn description(&self) -> &'static str {
                match self { $( Self::$variant => $description, )+ Self::Jump(_) => "Open a configured chat alias" }
            }
        }
    }
}

actions! {
    Quit => ("quit", "Exit Termgram"),
    ReloadConfig => ("reload_config", "Reload the Lua configuration atomically"),
    CommandLine => ("command", "Browse and run application commands"),
    PasteClipboard => ("paste_clipboard", "Paste clipboard files, an image or text into the draft"),
    Attach => ("attach", "Add files to the current draft"),
    Attachments => ("attachments", "Review the draft's attachments"),
    RemoveAttachment => ("remove_attachment", "Remove the selected draft attachment"),
    AttachmentFormat => ("attachment_format", "Switch between photo and original file"),
    CompleteNext => ("complete_next", "Complete the next command candidate"),
    CompletePrevious => ("complete_previous", "Complete the previous command candidate"),
    HistoryPrevious => ("history_previous", "Recall an older command with this prefix"),
    HistoryNext => ("history_next", "Recall a newer command with this prefix"),
    Help => ("help", "Show effective keyboard shortcuts"),
    Settings => ("settings", "Open application settings"),
    Accounts => ("accounts", "Switch Telegram accounts"),
    NextAccount => ("next_account", "Switch to the next account"),
    AddAccount => ("add_account", "Add a Telegram account"),
    Open => ("open", "Activate the selected item"),
    Preview => ("preview", "Expand the selected image or sticker"),
    Compose => ("compose", "Compose a message or reply to the selection"),
    ForwardMessage => ("forward_message", "Choose where to forward the selected message"),
    SaveMessage => ("save_message", "Forward the selected message to Saved Messages"),
    SavedMessages => ("saved_messages", "Open Saved Messages for this account"),
    CopyText => ("copy_text", "Copy the selected message text or caption"),
    CopyLink => ("copy_link", "Copy the selected Telegram message link"),
    DeleteMessage => ("delete_message", "Review the selected message and confirm its deletion scope"),
    EditMessage => ("edit_message", "Edit a message or resume a saved edit"),
    DiscardEdit => ("discard_edit", "Discard this chat’s local message edit"),
    Send => ("send", "Send the current draft"),
    Newline => ("newline", "Insert a new line"),
    Cancel => ("cancel", "Cancel or return to the previous view"),
    Focus => ("focus", "Switch pane focus"),
    Up => ("up", "Move up"),
    Down => ("down", "Move down"),
    MessageUp => ("message_up", "Select older messages"),
    MessageDown => ("message_down", "Select newer messages"),
    PageUp => ("page_up", "Move one page up"),
    PageDown => ("page_down", "Move one page down"),
    Oldest => ("oldest", "Go to the oldest loaded message"),
    Latest => ("latest", "Go to the latest messages"),
    FirstUnread => ("first_unread", "Go to the first unread message"),
    MarkRead => ("mark_read", "Mark this entire chat read"),
    MarkUnread => ("mark_unread", "Mark this chat unread as a reminder"),
    MuteChat => ("mute_chat", "Mute this chat on Telegram until unmuted"),
    UnmuteChat => ("unmute_chat", "Enable this chat's Telegram notifications"),
    Mentions => ("mentions", "Browse this chat's unread mentions and replies to you"),
    Filter => ("filter", "Filter chat titles"),
    Refresh => ("refresh", "Refresh chats and folders"),
    Reply => ("reply", "Reply to the selected message"),
    ReplyTarget => ("reply_target", "Open the original message"),
    OpenLink => ("open_link", "Open the selected link"),
    NextAction => ("next_action", "Select the next message action"),
    PreviousAction => ("previous_action", "Select the previous message action"),
    Reactions => ("reactions", "Choose an emoji reaction"),
    ClearReactions => ("clear_reactions", "Remove your emoji reactions from this message"),
    Stickers => ("stickers", "Open the sticker panel"),
    StickerSetNext => ("sticker_set_next", "Show the next sticker set"),
    StickerSetPrevious => ("sticker_set_previous", "Show the previous sticker set"),
    Poll => ("poll", "Read a poll and review your vote"),
    TogglePollAnswer => ("toggle_poll_answer", "Select or deselect this poll answer"),
    RetractVote => ("retract_vote", "Prepare to retract your poll vote"),
    Spoilers => ("spoilers", "Reveal or hide the selected message’s spoilers"),
    ExpandQuote => ("expand_quote", "Expand or collapse the selected message’s quote"),
    Reveal => ("reveal", "Reveal the selected file in the system file manager"),
    Redraw => ("redraw", "Redraw the terminal"),
    Home => ("home", "Move to the start of the input"),
    End => ("end", "Move to the end of the input"),
    Left => ("left", "Move left"),
    Right => ("right", "Move right"),
    Backspace => ("backspace", "Delete the previous character"),
    Delete => ("delete", "Delete the next character"),
    Clear => ("clear", "Clear the current input"),
    DeleteWord => ("delete_word", "Delete the previous word"),
    Noop => ("noop", "Remove this binding"),
    ChatColor => ("chat_color", "Choose a chat color"),
    FolderColor => ("folder_color", "Choose a folder color"),
    Search => ("search", "Search locally cached messages with a regex"),
    SearchScope => ("search_scope", "Change the search scope"),
    SearchMore => ("search_more", "Show the next search page"),
    SearchPrevious => ("search_previous", "Show the previous search page"),
    SearchQuery => ("search_query", "Edit the search query"),
    ChatInfo => ("chat_info", "Show chat and folder identifiers"),
    FolderNext => ("folder_next", "Switch to the next folder"),
    FolderPrevious => ("folder_previous", "Switch to the previous folder"),
    Pin => ("pin", "Change the selected Telegram pin"),
    PinUp => ("pin_up", "Move a pinned chat up"),
    PinDown => ("pin_down", "Move a pinned chat down"),
    Archive => ("archive", "Change the chat archive state"),
    Pins => ("pins", "Browse pinned messages"),
    PinsMore => ("pins_more", "Show the next pinned-message page"),
    PinsPrevious => ("pins_previous", "Show the previous pinned-message page"),
    UnpinAll => ("unpin_all", "Unpin all messages after confirmation"),
    ToggleSidebar => ("toggle_sidebar", "Show or hide the sidebar"),
}
