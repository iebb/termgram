//! Maps Telegram's unordered typing updates onto chat actions. These updates
//! carry no PTS and arrive on the existing update stream; nothing is polled.

use super::{NetworkEvent, PeerId, WorkerCache, tl};
use crate::activity::Action;
use tl::enums::SendMessageAction as Raw;

pub(super) fn event(update: &tl::enums::Update, cache: &WorkerCache) -> Option<NetworkEvent> {
    let (chat, sender, action) = match update {
        tl::enums::Update::UserTyping(update) => {
            let user = PeerId::user(update.user_id)?;
            (user, user, &update.action)
        }
        tl::enums::Update::ChatUserTyping(update) => (
            PeerId::chat(update.chat_id)?,
            PeerId::from(update.from_id.clone()),
            &update.action,
        ),
        tl::enums::Update::ChannelUserTyping(update) => (
            PeerId::channel(update.channel_id)?,
            PeerId::from(update.from_id.clone()),
            &update.action,
        ),
        _ => return None,
    };
    let chat_id = chat.bot_api_dialog_id()?;
    let sender_id = sender.bot_api_dialog_id()?;
    if sender_id == cache.self_id {
        return None;
    }
    let action = match action {
        Raw::SendMessageCancelAction => None,
        Raw::SendMessageTypingAction => Some(Action::Typing),
        Raw::SendMessageRecordVideoAction => Some(Action::RecordVideo),
        Raw::SendMessageUploadVideoAction(_) => Some(Action::UploadVideo),
        Raw::SendMessageRecordAudioAction => Some(Action::RecordVoice),
        Raw::SendMessageUploadAudioAction(_) => Some(Action::UploadVoice),
        Raw::SendMessageRecordRoundAction => Some(Action::RecordRound),
        Raw::SendMessageUploadRoundAction(_) => Some(Action::UploadRound),
        Raw::SendMessageUploadPhotoAction(_) => Some(Action::UploadPhoto),
        Raw::SendMessageUploadDocumentAction(_) => Some(Action::UploadFile),
        Raw::SendMessageGeoLocationAction => Some(Action::ChooseLocation),
        Raw::SendMessageChooseContactAction => Some(Action::ChooseContact),
        Raw::SendMessageChooseStickerAction => Some(Action::ChooseSticker),
        Raw::SendMessageGamePlayAction => Some(Action::PlayGame),
        Raw::SpeakingInGroupCallAction => Some(Action::Speaking),
        // Emoji interactions, history imports and streamed drafts are not
        // presence; Desktop does not show them as a chat action either.
        _ => return None,
    };
    Some(NetworkEvent::ChatAction {
        chat_id,
        sender_id,
        sender: cache.names.get(&sender).cloned(),
        action,
    })
}
