//! Durable user-authored state, separate from the discardable message cache.
//! SQLite owns atomic replacement; the writer coalesces snapshots off the UI thread.

use std::{collections::BTreeMap, path::Path, time::Duration};

use anyhow::{Context, Result, ensure};
use libsql::{Connection, Database, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::{
    config::Config,
    input::TextInput,
    model::{ChatId, ReplyInfo, sanitize_terminal_text},
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Key {
    pub account: i64,
    pub chat: ChatId,
    /// Zero represents the main conversation; positive IDs identify topics.
    pub topic: i32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Draft {
    pub input: TextInput,
    pub edit: Option<crate::editing::Draft>,
    pub reply: Option<ReplyInfo>,
    pub attachments: Vec<crate::staging::Attachment>,
}

impl Draft {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.input.is_empty()
            && self.reply.is_none()
            && self.attachments.is_empty()
            && self.edit.is_none()
    }

    #[must_use]
    pub fn stored(&self, chat: ChatId, topic: i32) -> Stored {
        Stored {
            chat,
            topic,
            text: self.input.value().to_owned(),
            cursor: self.input.cursor(),
            edit: self.edit.as_ref().map(crate::editing::Draft::stored),
            reply: self.reply.clone(),
            attachments: self.attachments.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Stored {
    pub chat: ChatId,
    pub topic: i32,
    pub text: String,
    pub cursor: usize,
    #[serde(default)]
    pub edit: Option<crate::editing::Stored>,
    pub reply: Option<ReplyInfo>,
    #[serde(default)]
    pub attachments: Vec<crate::staging::Attachment>,
}

impl Stored {
    #[must_use]
    pub fn draft(&self) -> Draft {
        Draft {
            input: TextInput::with_cursor(sanitize_terminal_text(&self.text), self.cursor),
            edit: self.edit.as_ref().map(crate::editing::Stored::draft),
            reply: self.reply.clone(),
            attachments: self.attachments.clone(),
        }
    }
}

/// Only loaded accounts are present, including those with no remaining drafts.
/// Unknown accounts must never be interpreted as a request to erase their data.
pub type Snapshot = BTreeMap<i64, Vec<Stored>>;

/// Load before exposing an account's cached conversations, without making a
/// SQLite lock wait block terminal input on the current-thread UI runtime.
/// # Errors
/// Returns the original storage/decoding failure or a reader-task failure.
pub async fn load(path: std::path::PathBuf, account: i64) -> Result<Vec<Stored>> {
    tokio::task::spawn_blocking(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let mut drafts = Store::open(&path).await?.load(account).await?;
                crate::clipboard::restore(&path, account, &mut drafts)?;
                Ok(drafts)
            })
    })
    .await
    .context("draft reader stopped unexpectedly")?
}

pub struct Store {
    _database: Database,
    connection: Connection,
}

impl Store {
    /// # Errors
    /// Returns storage/migration errors without discarding existing drafts.
    pub async fn open(path: &Path) -> Result<Self> {
        crate::config::prepare_private_file(path)?;
        let database = libsql::Builder::new_local(path).build().await?;
        let connection = database.connect()?;
        connection.busy_timeout(Duration::from_secs(5))?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let version = transaction
            .query("PRAGMA user_version", ())
            .await?
            .next()
            .await?
            .context("missing draft schema version")?
            .get::<i64>(0)?;
        ensure!(
            version <= 3,
            "drafts were created by a newer Termgram version"
        );
        if version == 0 {
            transaction.execute_batch("CREATE TABLE drafts(account_id INTEGER NOT NULL, chat_id INTEGER NOT NULL, topic_id INTEGER NOT NULL, data TEXT NOT NULL, PRIMARY KEY(account_id,chat_id,topic_id)); PRAGMA user_version=1;").await?;
        }
        if version < 2 {
            transaction.execute_batch("PRAGMA user_version=2;").await?;
        }
        if version < 3 {
            transaction.execute_batch("PRAGMA user_version=3;").await?;
        }
        transaction.commit().await?;
        Ok(Self {
            _database: database,
            connection,
        })
    }

    /// # Errors
    /// Returns a database or draft decoding error.
    pub async fn load(&self, account: i64) -> Result<Vec<Stored>> {
        let mut rows = self
            .connection
            .query(
                "SELECT data FROM drafts WHERE account_id=?1 ORDER BY chat_id,topic_id",
                [account],
            )
            .await?;
        let mut drafts = Vec::new();
        while let Some(row) = rows.next().await? {
            drafts.push(serde_json::from_str(&row.get::<String>(0)?)?);
        }
        Ok(drafts)
    }

    /// # Errors
    /// A failed transaction leaves the previous account drafts intact.
    pub async fn save(&self, snapshot: &Snapshot, previous: &Snapshot) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        for (&account, drafts) in snapshot {
            if previous.get(&account) == Some(drafts) {
                continue;
            }
            ensure!(account > 0, "drafts require a Telegram user identity");
            transaction
                .execute("DELETE FROM drafts WHERE account_id=?1", [account])
                .await?;
            for draft in drafts {
                transaction
                    .execute(
                        "INSERT INTO drafts VALUES(?1,?2,?3,?4)",
                        params![
                            account,
                            draft.chat,
                            draft.topic,
                            serde_json::to_string(draft)?
                        ],
                    )
                    .await?;
            }
        }
        transaction.commit().await?;
        Ok(())
    }
}

pub struct Writer {
    snapshots: watch::Sender<Snapshot>,
    errors: watch::Receiver<Option<String>>,
    task: tokio::task::JoinHandle<Result<()>>,
}

impl Writer {
    #[must_use]
    pub fn spawn(config: Config) -> Self {
        let (snapshots, receiver) = watch::channel(Snapshot::new());
        let (errors_tx, errors) = watch::channel(None);
        let task = tokio::task::spawn_blocking(move || {
            let result = (|| -> Result<()> {
                config.prepare_session_dir()?;
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?
                    .block_on(write_loop(&config.state_path, receiver, &errors_tx))
            })();
            if let Err(error) = &result {
                errors_tx.send_replace(Some(format!("Draft storage: {error:#}")));
            }
            result
        });
        Self {
            snapshots,
            errors,
            task,
        }
    }

    pub fn queue(&self, snapshot: Snapshot) {
        self.snapshots.send_replace(snapshot);
    }

    pub async fn next_error(&mut self) -> String {
        loop {
            if self.errors.changed().await.is_err() {
                return std::future::pending().await;
            }
            if let Some(error) = self.errors.borrow_and_update().clone() {
                return error;
            }
        }
    }

    /// Flush the last coalesced snapshot before completing normal shutdown.
    /// # Errors
    /// Returns the final write failure instead of claiming the draft was saved.
    pub async fn finish(self) -> Result<()> {
        drop(self.snapshots);
        self.task
            .await
            .context("draft writer stopped unexpectedly")?
    }
}

async fn write_loop(
    path: &Path,
    mut snapshots: watch::Receiver<Snapshot>,
    errors: &watch::Sender<Option<String>>,
) -> Result<()> {
    let store = Store::open(path).await?;
    let mut previous = Snapshot::new();
    while snapshots.changed().await.is_ok() {
        // A fixed coalescing window also bounds saves during continuous typing.
        tokio::time::sleep(Duration::from_millis(400)).await;
        loop {
            let snapshot = snapshots.borrow_and_update().clone();
            match store.save(&snapshot, &previous).await {
                Ok(()) => {
                    previous = snapshot;
                    break;
                }
                Err(error) => {
                    errors.send_replace(Some(format!("Draft not saved; retrying: {error:#}")));
                    if snapshots.has_changed().is_err() {
                        return Err(error);
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }
    let last = snapshots.borrow_and_update().clone();
    store.save(&last, &previous).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(chat: ChatId, topic: i32, text: &str) -> Stored {
        Stored {
            edit: None,
            attachments: Vec::new(),
            chat,
            topic,
            text: text.to_owned(),
            cursor: 1,
            reply: Some(ReplyInfo {
                chat_id: chat,
                message_id: 77,
                sender: Some("Full sender name".to_owned()),
            }),
        }
    }

    #[tokio::test]
    async fn drafts_survive_restart_and_failed_replacement_without_crossing_accounts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite3");
        let store = Store::open(&path).await.unwrap();
        let mut initial = Snapshot::from([
            (
                100,
                vec![stored(7, 0, "a界🙂"), stored(7, 44, "Topic draft")],
            ),
            (200, vec![stored(7, 0, "Other account")]),
        ]);
        initial.get_mut(&100).unwrap()[0]
            .attachments
            .push(crate::staging::Attachment {
                path: directory.path().join("attachment.txt"),
                size: 12,
                digest: [7; 32],
                as_photo: false,
                photo_supported: false,
                owned: false,
                lease: None,
            });
        initial.get_mut(&100).unwrap()[0].edit = Some(crate::editing::Stored {
            source: crate::editing::Source {
                message_id: 41,
                text: "original".to_owned(),
                revision: [1; 32],
                caption: true,
            },
            text: "modified caption 界🙂".to_owned(),
            cursor: 18,
        });
        store.save(&initial, &Snapshot::new()).await.unwrap();
        let duplicate = stored(7, 0, "duplicate");
        assert!(
            store
                .save(
                    &Snapshot::from([(100, vec![duplicate.clone(), duplicate])]),
                    &initial
                )
                .await
                .is_err()
        );
        assert_eq!(store.load(100).await.unwrap(), initial[&100]);
        drop(store);
        let restored = load(path.clone(), 100).await.unwrap();
        assert_eq!(restored, initial[&100]);
        assert_eq!(restored[0].draft().input.cursor(), 1);
        assert_eq!(TextInput::with_cursor("a界🙂", 2).cursor(), 1);
        let store = Store::open(&path).await.unwrap();
        store
            .save(&Snapshot::from([(100, Vec::new())]), &initial)
            .await
            .unwrap();
        assert!(store.load(100).await.unwrap().is_empty());
        assert_eq!(store.load(200).await.unwrap(), initial[&200]);
    }

    #[tokio::test]
    async fn normal_shutdown_flushes_the_latest_coalesced_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite3");
        let writer = Writer::spawn(Config {
            api_id: 1,
            api_hash: "unused".to_owned(),
            measure_latency: false,
            session_path: directory.path().join("unused.session"),
            state_path: path.clone(),
            proxy: None,
        });
        writer.queue(Snapshot::from([(100, vec![stored(7, 0, "first")])]));
        let final_snapshot = Snapshot::from([
            (100, vec![stored(7, 0, "latest")]),
            (200, vec![stored(7, 0, "separate")]),
        ]);
        writer.queue(final_snapshot.clone());
        writer.finish().await.unwrap();
        let store = Store::open(&path).await.unwrap();
        assert_eq!(store.load(100).await.unwrap(), final_snapshot[&100]);
        assert_eq!(store.load(200).await.unwrap(), final_snapshot[&200]);
    }
}
