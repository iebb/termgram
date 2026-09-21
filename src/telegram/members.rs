//! Member and bot-command lookup backing composer `@`/`/` completion.
//!
//! One bounded snapshot per chat is enough for filtering in the composer:
//! megagroups use Telegram's mentionable-participant filter, small groups
//! report their full roster, and direct chats resolve the peer's own handle
//! plus any advertised bot commands.

use anyhow::Result;
use grammers_client::{Client, peer::User, tl};
use grammers_session::types::{PeerKind, PeerRef};

use crate::completion::{BotCommand, ChatCompletion, Member};

/// One filtered page is the same suggestion set official clients use.
const MEMBER_LIMIT: usize = 200;
/// Fetching every bot profile could flood; completion rarely needs more.
const BOT_LOOKUP_LIMIT: usize = 8;

pub(super) async fn load(client: &Client, peer: PeerRef, self_id: i64) -> Result<ChatCompletion> {
    if peer.id.kind() == PeerKind::User {
        return user_completion(client, peer, self_id).await;
    }
    let mut iter = client.iter_participants(peer);
    if peer.id.kind() == PeerKind::Channel {
        iter = iter.filter(
            tl::enums::ChannelParticipantsFilter::ChannelParticipantsMentions(
                tl::types::ChannelParticipantsMentions {
                    q: Some(String::new()),
                    top_msg_id: None,
                },
            ),
        );
    }
    let mut completion = ChatCompletion::default();
    let mut bots = Vec::new();
    while completion.members.len() < MEMBER_LIMIT {
        let Some(participant) = iter.next().await? else {
            break;
        };
        let user = participant.user;
        if user.id().bare_id_unchecked() == self_id {
            continue;
        }
        if user.is_bot()
            && bots.len() < BOT_LOOKUP_LIMIT
            && let Ok(Some(reference)) = user.to_ref().await
        {
            bots.push((reference, user.username().map(str::to_owned)));
        }
        if let Some(member) = member(&user) {
            completion.members.push(member);
        }
    }
    for (reference, username) in bots {
        // A single bot's profile must not take down the whole member list.
        if let Ok(bot_commands) = bot_commands(client, reference).await {
            collect_commands(bot_commands, username.as_deref(), &mut completion.commands);
        }
    }
    Ok(completion)
}

/// A direct chat offers the peer's own handle for `@` and, for bots, its
/// advertised commands.
async fn user_completion(client: &Client, peer: PeerRef, self_id: i64) -> Result<ChatCompletion> {
    let tl::enums::users::UserFull::Full(full) = client
        .invoke(&tl::functions::users::GetFullUser { id: peer.into() })
        .await?;
    let tl::enums::UserFull::Full(full_user) = full.full_user;
    let mut completion = ChatCompletion::default();
    let Some(raw) = full.users.into_iter().next() else {
        return Ok(completion);
    };
    let user = User::from_raw(client, raw);
    if user.id().bare_id_unchecked() != self_id
        && let Some(member) = member(&user)
    {
        completion.members.push(member);
    }
    if user.is_bot() {
        collect_commands(
            full_user.bot_info,
            user.username(),
            &mut completion.commands,
        );
    }
    Ok(completion)
}

fn member(user: &User) -> Option<Member> {
    Some(Member {
        name: crate::model::sanitize_terminal_line(&user.full_name()),
        username: user.username()?.to_owned(),
        bot: user.is_bot(),
    })
}

async fn bot_commands(client: &Client, bot: PeerRef) -> Result<Option<tl::enums::BotInfo>> {
    let tl::enums::users::UserFull::Full(full) = client
        .invoke(&tl::functions::users::GetFullUser { id: bot.into() })
        .await?;
    let tl::enums::UserFull::Full(full_user) = full.full_user;
    Ok(full_user.bot_info)
}

fn collect_commands(
    info: Option<tl::enums::BotInfo>,
    username: Option<&str>,
    out: &mut Vec<BotCommand>,
) {
    let Some(tl::enums::BotInfo::Info(info)) = info else {
        return;
    };
    for command in info.commands.iter().flatten() {
        let tl::enums::BotCommand::Command(command) = command;
        if command.command.is_empty() {
            continue;
        }
        out.push(BotCommand {
            bot: username.map(str::to_owned),
            command: command.command.clone(),
            description: command.description.clone(),
        });
    }
}
