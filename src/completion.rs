//! Composer completion data and token detection.
//!
//! `@` completes mentionable chat members, `/` completes the commands that
//! bots in the chat advertise. Member data is loaded once per chat on demand
//! and merged with senders already visible in the loaded transcript, so the
//! popup works before and without the network fetch.

/// A chat participant that can be addressed through `@username`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Member {
    pub name: String,
    /// Public handle without the leading `@`; required for text mentions.
    pub username: String,
    pub bot: bool,
}

/// A slash command advertised by a bot present in the chat.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BotCommand {
    /// Bot username without `@`, used to disambiguate shared command names.
    pub bot: Option<String>,
    /// Command name without the leading `/`.
    pub command: String,
    pub description: String,
}

/// Member and command data fetched for one chat.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ChatCompletion {
    pub members: Vec<Member>,
    pub commands: Vec<BotCommand>,
}

/// Which token the composer is completing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Trigger {
    /// `@…` — chat member mention.
    Mention,
    /// `/…` — bot command, only at the start of the draft.
    Command,
}

/// One row in the completion popup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Suggestion {
    /// Replacement text for the token, without a trailing space.
    pub insert: String,
    /// Primary label, for example `@alice` or `/start`.
    pub label: String,
    /// Secondary text: display name or command description.
    pub detail: String,
}

/// Find the completable token ending at `cursor` and return its trigger kind
/// plus the byte offset of the trigger character. The query is the text in
/// `value[start + 1..cursor]`.
///
/// `@` must sit at the start of the draft or directly after whitespace, like
/// official clients; `/` only completes as the very first character. Queries
/// use Telegram's handle alphabet extended with Unicode letters/digits so
/// members can also be filtered by their display names.
#[must_use]
pub fn token(value: &str, cursor: usize) -> Option<(Trigger, usize)> {
    let prefix = value.get(..cursor)?;
    let query_bytes: usize = prefix
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .map(char::len_utf8)
        .sum();
    let start = cursor.checked_sub(query_bytes + 1)?;
    match *prefix.as_bytes().get(start)? {
        b'@' if prefix[..start]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace) =>
        {
            Some((Trigger::Mention, start))
        }
        b'/' if start == 0 => Some((Trigger::Command, start)),
        _ => None,
    }
}

/// Rank members for a query: username prefixes first, then name prefixes,
/// then other substring matches. Order inside each rank is preserved, keeping
/// the server's recent-first ordering. Members repeating a username are dropped.
#[must_use]
pub fn mention_suggestions(
    members: impl IntoIterator<Item = Member>,
    query: &str,
) -> Vec<Suggestion> {
    let query = query.to_lowercase();
    let mut seen = std::collections::HashSet::new();
    let mut ranked = members
        .into_iter()
        .filter(|member| !member.username.is_empty())
        .filter(|member| seen.insert(member.username.to_lowercase()))
        .filter_map(|member| {
            let username = member.username.to_lowercase();
            let name = member.name.to_lowercase();
            let rank = if username.starts_with(&query) {
                0
            } else if name.starts_with(&query) {
                1
            } else if username.contains(&query) || name.contains(&query) {
                2
            } else {
                return None;
            };
            Some((
                rank,
                Suggestion {
                    insert: format!("@{}", member.username),
                    label: format!("@{}", member.username),
                    detail: member.name,
                },
            ))
        })
        .collect::<Vec<_>>();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.truncate(50);
    ranked
        .into_iter()
        .map(|(_, suggestion)| suggestion)
        .collect()
}

/// Commands whose name starts with the query. When several bots advertise
/// commands, the insertion gains an explicit `@bot` suffix so Telegram routes
/// it unambiguously — matching official client behavior.
#[must_use]
pub fn command_suggestions(
    commands: &[BotCommand],
    query: &str,
    disambiguate: bool,
) -> Vec<Suggestion> {
    let query = query.to_lowercase();
    commands
        .iter()
        .filter(|entry| entry.command.to_lowercase().starts_with(&query))
        .map(|entry| {
            let suffix = if disambiguate {
                entry.bot.as_deref().unwrap_or_default()
            } else {
                ""
            };
            let suffix = if suffix.is_empty() {
                String::new()
            } else {
                format!("@{suffix}")
            };
            Suggestion {
                insert: format!("/{}{}", entry.command, suffix),
                label: format!("/{}{}", entry.command, suffix),
                detail: match (
                    entry.bot.as_ref().filter(|_| disambiguate),
                    entry.description.is_empty(),
                ) {
                    (Some(bot), false) => format!("@{bot} · {}", entry.description),
                    (Some(bot), true) => format!("@{bot}"),
                    (None, false) => entry.description.clone(),
                    (None, true) => String::new(),
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{BotCommand, Member, Trigger, command_suggestions, mention_suggestions, token};

    fn member(name: &str, username: &str) -> Member {
        Member {
            name: name.to_owned(),
            username: username.to_owned(),
            bot: false,
        }
    }

    #[test]
    fn detects_mention_and_command_tokens() {
        assert_eq!(token("@", 1), Some((Trigger::Mention, 0)));
        assert_eq!(token("hello @al", 9), Some((Trigger::Mention, 6)));
        assert_eq!(token("\n@界", 5), Some((Trigger::Mention, 1)));
        assert_eq!(token("mail@ex", 7), None);
        assert_eq!(token("a@b @c", 6), Some((Trigger::Mention, 4)));
        assert_eq!(token("/", 1), Some((Trigger::Command, 0)));
        assert_eq!(token("/sta", 4), Some((Trigger::Command, 0)));
        assert_eq!(token(" /sta", 5), None);
        assert_eq!(token("/cmd x", 6), None);
        assert_eq!(token("/cmd@bo", 7), None);
        assert_eq!(token("", 0), None);
        // Cursor in the middle of a token still completes its left side.
        assert_eq!(token("@alice rest", 4), Some((Trigger::Mention, 0)));
    }

    #[test]
    fn ranks_and_deduplicates_mentions() {
        let members = vec![
            member("Bob", "bobby"),
            member("Alice A", "alice"),
            member("Alice B", "alina"),
            member("Alice A", "alice"),
            member("No handle", ""),
        ];
        let rows = mention_suggestions(members.clone(), "al");
        assert_eq!(
            rows.iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            ["@alice", "@alina"]
        );
        assert_eq!(rows[0].insert, "@alice");
        // A display name also matches; username prefixes still rank first.
        let rows = mention_suggestions(
            vec![
                member("Bob", "bobby"),
                member("Zed", "zed"),
                member("Bob", "x"),
            ],
            "bo",
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "@bobby");
        // An empty query lists every handle once.
        assert_eq!(mention_suggestions(members.clone(), "").len(), 3);
        // Chinese display names participate in filtering.
        assert_eq!(
            mention_suggestions(vec![member("小明", "xiaoming")], "明").len(),
            1
        );
    }

    #[test]
    fn completes_bot_commands_with_optional_disambiguation() {
        let commands = vec![
            BotCommand {
                bot: Some("botfather".to_owned()),
                command: "newbot".to_owned(),
                description: "Create a bot".to_owned(),
            },
            BotCommand {
                bot: Some("other".to_owned()),
                command: "newbot".to_owned(),
                description: "Duplicate".to_owned(),
            },
            BotCommand {
                bot: Some("botfather".to_owned()),
                command: "help".to_owned(),
                description: "Show help".to_owned(),
            },
        ];
        let rows = command_suggestions(&commands, "new", true);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].insert, "/newbot@botfather");
        assert!(rows[0].detail.contains("@botfather"));
        let rows = command_suggestions(&commands, "h", false);
        assert_eq!(rows[0].insert, "/help");
        assert_eq!(rows[0].detail, "Show help");
        assert!(command_suggestions(&commands, "zzz", false).is_empty());
    }
}
