// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::collections::HashMap;
use std::fmt;
use std::net::AddrParseError;
use std::path::Path;
use std::sync::Mutex;

use libsql::{named_params, params};
use tokio::sync::Mutex as AsyncMutex;

use crate::types::{
    ChannelKind, ChannelState, DcOption, PeerAuth, PeerId, PeerInfo, PeerKind, UpdateState,
    UpdatesState,
};
use crate::{BoxFuture, DEFAULT_DC, KNOWN_DC_OPTIONS, Session};

const VERSION: i64 = 1;

struct Database(libsql::Connection);

struct Cache {
    pub home_dc: i32,
    pub dc_options: HashMap<i32, DcOption>,
}

/// SQLite-based storage. This is the recommended option.
pub struct SqliteSession {
    database: AsyncMutex<Database>,
    cache: Mutex<Cache>,
}

#[derive(Debug)]
pub enum SqliteSessionError {
    Poisoned,
    AddrParse(std::net::AddrParseError),
    Sql(libsql::Error),
    InvalidAuthKeyLength(usize),
}

impl std::error::Error for SqliteSessionError {}

impl fmt::Display for SqliteSessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SqliteSessionError::Poisoned => write!(f, "session lock is poisoned"),
            SqliteSessionError::AddrParse(_) => write!(f, "invalid socket address syntax"),
            SqliteSessionError::Sql(err) => write!(f, "{err}"),
            SqliteSessionError::InvalidAuthKeyLength(actual) => {
                write!(f, "invalid auth_key length: expected 256, got {actual}")
            }
        }
    }
}

impl From<AddrParseError> for SqliteSessionError {
    fn from(x: AddrParseError) -> Self {
        Self::AddrParse(x)
    }
}

impl From<libsql::Error> for SqliteSessionError {
    fn from(x: libsql::Error) -> Self {
        Self::Sql(x)
    }
}

#[repr(u8)]
enum PeerSubtype {
    UserSelf = 1,
    UserBot = 2,
    UserSelfBot = 3,
    Megagroup = 4,
    Broadcast = 8,
    Gigagroup = 12,
}

impl Database {
    async fn init(&self) -> libsql::Result<()> {
        // Serialize schema inspection and creation across processes. The version
        // belongs to the same transaction as the schema it describes.
        let transaction = self.begin_transaction().await?;
        let user_version: i64 = self
            .fetch_one("PRAGMA user_version", params![], |row| row.get(0))
            .await?
            .unwrap_or(0);
        if user_version == 0 {
            self.migrate_v0_to_v1(&transaction).await?;
            transaction
                .execute(&format!("PRAGMA user_version = {VERSION}"), params![])
                .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    async fn migrate_v0_to_v1(&self, transaction: &libsql::Transaction) -> libsql::Result<()> {
        transaction
            .execute(
                "CREATE TABLE dc_home (
                dc_id INTEGER NOT NULL,
                PRIMARY KEY(dc_id))",
                params![],
            )
            .await?;
        transaction
            .execute(
                "CREATE TABLE dc_option (
                dc_id INTEGER NOT NULL,
                ipv4 TEXT NOT NULL,
                ipv6 TEXT NOT NULL,
                auth_key BLOB,
                PRIMARY KEY (dc_id))",
                params![],
            )
            .await?;
        transaction
            .execute(
                "CREATE TABLE peer_info (
                peer_id INTEGER NOT NULL,
                hash INTEGER,
                subtype INTEGER,
                PRIMARY KEY (peer_id))",
                params![],
            )
            .await?;
        transaction
            .execute(
                "CREATE TABLE update_state (
                pts INTEGER NOT NULL,
                qts INTEGER NOT NULL,
                date INTEGER NOT NULL,
                seq INTEGER NOT NULL)",
                params![],
            )
            .await?;
        transaction
            .execute(
                "CREATE TABLE channel_state (
                peer_id INTEGER NOT NULL,
                pts INTEGER NOT NULL,
                PRIMARY KEY (peer_id))",
                params![],
            )
            .await?;

        Ok(())
    }

    async fn begin_transaction(&self) -> libsql::Result<libsql::Transaction> {
        self.0.transaction_with_behavior(libsql::TransactionBehavior::Immediate).await
    }

    async fn fetch_one<
        T,
        P: libsql::params::IntoParams,
        F: FnOnce(libsql::Row) -> libsql::Result<T>,
    >(
        &self,
        statement: &str,
        params: P,
        select: F,
    ) -> libsql::Result<Option<T>> {
        let mut statement = self.0.prepare(statement).await?;
        let result = statement.query_row(params).await;
        match result {
            Ok(value) => Ok(Some(select(value)?)),
            Err(libsql::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn fetch_all<
        T,
        P: libsql::params::IntoParams,
        F: FnMut(libsql::Row) -> Result<T, SqliteSessionError>,
    >(
        &self,
        statement: &str,
        params: P,
        mut select: F,
    ) -> Result<Vec<T>, SqliteSessionError> {
        let statement = self.0.prepare(statement).await?;
        let mut rows = statement.query(params).await?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().await? {
            result.push(select(row)?);
        }
        Ok(result)
    }
}

impl SqliteSession {
    /// Open a connection to the SQLite database at `path`,
    /// creating one if it doesn't exist.
    pub async fn open<P: AsRef<Path>>(path: P) -> Result<Self, SqliteSessionError> {
        let conn = libsql::Builder::new_local(path).build().await?.connect()?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let db = Database(conn);
        db.init().await?;

        let home_dc = db
            .fetch_one("SELECT * FROM dc_home LIMIT 1", named_params![], |row| {
                Ok(row.get::<i32>(0)?)
            })
            .await?
            .unwrap_or(DEFAULT_DC);

        let dc_options = db
            .fetch_all("SELECT * FROM dc_option", named_params![], |row| {
                Ok(DcOption {
                    id: row.get::<i32>(0)?,
                    ipv4: row.get::<String>(1)?.parse()?,
                    ipv6: row.get::<String>(2)?.parse()?,
                    auth_key: match row.get::<Option<Vec<u8>>>(3)? {
                        None => None,
                        Some(auth_key) => Some(auth_key.try_into().map_err(|v: Vec<u8>| {
                            SqliteSessionError::InvalidAuthKeyLength(v.len())
                        })?),
                    },
                })
            })
            .await?
            .into_iter()
            .map(|dc_option| (dc_option.id, dc_option))
            .collect();

        Ok(SqliteSession {
            database: AsyncMutex::new(db),
            cache: Mutex::new(Cache {
                home_dc,
                dc_options,
            }),
        })
    }
}

impl Session for SqliteSession {
    type Error = SqliteSessionError;

    fn home_dc_id(&self) -> Result<i32, SqliteSessionError> {
        Ok(self
            .cache
            .lock()
            .map_err(|_| SqliteSessionError::Poisoned)?
            .home_dc)
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), SqliteSessionError>> {
        let ok = match self.cache.lock() {
            Err(_) => Err(SqliteSessionError::Poisoned),
            Ok(mut x) => {
                x.home_dc = dc_id;
                Ok(())
            }
        };
        Box::pin(async move {
            ok?;

            let db = self.database.lock().await;
            let transaction = db.begin_transaction().await?;
            transaction
                .execute("DELETE FROM dc_home", params![])
                .await?;
            let stmt = transaction
                .prepare("INSERT INTO dc_home VALUES (:dc_id)")
                .await?;
            stmt.execute(named_params! {":dc_id": dc_id}).await?;
            transaction.commit().await?;
            Ok(())
        })
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, SqliteSessionError> {
        Ok(self
            .cache
            .lock()
            .map_err(|_| SqliteSessionError::Poisoned)?
            .dc_options
            .get(&dc_id)
            .cloned()
            .or_else(|| {
                KNOWN_DC_OPTIONS
                    .iter()
                    .find(|dc_option| dc_option.id == dc_id)
                    .cloned()
            }))
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, Result<(), SqliteSessionError>> {
        let ok = match self.cache.lock() {
            Err(_) => Err(SqliteSessionError::Poisoned),
            Ok(mut x) => {
                x.dc_options.insert(dc_option.id, dc_option.clone());
                Ok(())
            }
        };

        let dc_option = dc_option.clone();
        Box::pin(async move {
            ok?;

            let db = self.database.lock().await;
            db.0.execute(
                "INSERT OR REPLACE INTO dc_option VALUES (:dc_id, :ipv4, :ipv6, :auth_key)",
                named_params! {
                    ":dc_id": dc_option.id,
                    ":ipv4": dc_option.ipv4.to_string(),
                    ":ipv6": dc_option.ipv6.to_string(),
                    ":auth_key": dc_option.auth_key.map(|k| k.to_vec()),
                },
            )
            .await?;
            Ok(())
        })
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, SqliteSessionError>> {
        Box::pin(async move {
            let db = self.database.lock().await;
            let map_row = |row: libsql::Row| {
                let subtype = row.get::<Option<i64>>(2)?.map(|s| s as u8);
                Ok(match peer.kind() {
                    PeerKind::User => PeerInfo::User {
                        id: PeerId::user_unchecked(row.get::<i64>(0)?).bare_id_unchecked(),
                        auth: row.get::<Option<i64>>(1)?.map(PeerAuth::from_hash),
                        bot: subtype.map(|s| s & PeerSubtype::UserBot as u8 != 0),
                        is_self: subtype.map(|s| s & PeerSubtype::UserSelf as u8 != 0),
                    },
                    PeerKind::Chat => PeerInfo::Chat {
                        id: peer.bare_id_unchecked(),
                    },
                    PeerKind::Channel => PeerInfo::Channel {
                        id: peer.bare_id_unchecked(),
                        auth: row.get::<Option<i64>>(1)?.map(PeerAuth::from_hash),
                        kind: subtype.and_then(|s| {
                            if (s & PeerSubtype::Gigagroup as u8) == PeerSubtype::Gigagroup as u8 {
                                Some(ChannelKind::Gigagroup)
                            } else if s & PeerSubtype::Broadcast as u8 != 0 {
                                Some(ChannelKind::Broadcast)
                            } else if s & PeerSubtype::Megagroup as u8 != 0 {
                                Some(ChannelKind::Megagroup)
                            } else {
                                None
                            }
                        }),
                    },
                })
            };

            Ok(if let Some(peer_id) = peer.bot_api_dialog_id() {
                db.fetch_one(
                    "SELECT * FROM peer_info WHERE peer_id = :peer_id LIMIT 1",
                    named_params! {":peer_id": peer_id},
                    map_row,
                )
                .await?
            } else {
                db.fetch_one(
                    "SELECT * FROM peer_info WHERE subtype & :type LIMIT 1",
                    named_params! {":type": PeerSubtype::UserSelf as i64},
                    map_row,
                )
                .await?
            })
        })
    }

    fn cache_peer(&self, peer: &PeerInfo) -> BoxFuture<'_, Result<(), SqliteSessionError>> {
        let peer = peer.clone();
        Box::pin(async move {
            let peer = if let Some(mut existing_peer) = self.peer(peer.id()).await? {
                existing_peer.extend_info(&peer);
                existing_peer
            } else {
                peer
            };

            let db = self.database.lock().await;
            let stmt =
                db.0.prepare("INSERT OR REPLACE INTO peer_info VALUES (:peer_id, :hash, :subtype)")
                    .await?;
            let subtype = match peer {
                PeerInfo::User { bot, is_self, .. } => {
                    match (bot.unwrap_or_default(), is_self.unwrap_or_default()) {
                        (true, true) => Some(PeerSubtype::UserSelfBot),
                        (true, false) => Some(PeerSubtype::UserBot),
                        (false, true) => Some(PeerSubtype::UserSelf),
                        (false, false) => None,
                    }
                }
                PeerInfo::Chat { .. } => None,
                PeerInfo::Channel { kind, .. } => kind.map(|kind| match kind {
                    ChannelKind::Megagroup => PeerSubtype::Megagroup,
                    ChannelKind::Broadcast => PeerSubtype::Broadcast,
                    ChannelKind::Gigagroup => PeerSubtype::Gigagroup,
                }),
            };
            let mut params = vec![];
            let peer_id = peer.id().bot_api_dialog_id_unchecked();
            params.push((":peer_id".to_owned(), peer_id));
            let hash = peer.auth().unwrap_or_default().hash();
            if peer.auth().is_some() {
                params.push((":hash".to_owned(), hash));
            }
            let subtype = subtype.map(|s| s as i64);
            if subtype.is_some() {
                params.push((":subtype".to_owned(), subtype.unwrap()));
            }
            stmt.execute(params).await?;
            Ok(())
        })
    }

    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, SqliteSessionError>> {
        Box::pin(async move {
            let db = self.database.lock().await;
            let mut state = db
                .fetch_one(
                    "SELECT * FROM update_state LIMIT 1",
                    named_params![],
                    |row| {
                        Ok(UpdatesState {
                            pts: row.get(0)?,
                            qts: row.get(1)?,
                            date: row.get(2)?,
                            seq: row.get(3)?,
                            channels: Vec::new(),
                        })
                    },
                )
                .await?
                .unwrap_or_default();
            state.channels = db
                .fetch_all("SELECT * FROM channel_state", named_params![], |row| {
                    Ok(ChannelState {
                        id: row.get(0)?,
                        pts: row.get(1)?,
                    })
                })
                .await?;
            Ok(state)
        })
    }

    fn set_update_state(
        &self,
        update: UpdateState,
    ) -> BoxFuture<'_, Result<(), SqliteSessionError>> {
        Box::pin(async move {
            let db = self.database.lock().await;
            let transaction = db.begin_transaction().await?;

            match update {
                UpdateState::All(updates_state) => {
                    transaction
                        .execute("DELETE FROM update_state", params![])
                        .await?;
                    transaction
                        .execute(
                            "INSERT INTO update_state VALUES (:pts, :qts, :date, :seq)",
                            named_params! {
                                ":pts": updates_state.pts,
                                ":qts": updates_state.qts,
                                ":date": updates_state.date,
                                ":seq": updates_state.seq,
                            },
                        )
                        .await?;

                    transaction
                        .execute("DELETE FROM channel_state", params![])
                        .await?;
                    for channel in updates_state.channels {
                        transaction
                            .execute(
                                "INSERT INTO channel_state VALUES (:peer_id, :pts)",
                                named_params! {
                                    ":peer_id": channel.id,
                                    ":pts": channel.pts,
                                },
                            )
                            .await?;
                    }
                }
                UpdateState::Primary { pts, date, seq } => {
                    let previous = db
                        .fetch_one(
                            "SELECT * FROM update_state LIMIT 1",
                            named_params![],
                            |_| Ok(()),
                        )
                        .await?;

                    if previous.is_some() {
                        transaction
                            .execute(
                                "UPDATE update_state SET pts = :pts, date = :date, seq = :seq",
                                named_params! {
                                    ":pts": pts,
                                    ":date": date,
                                    ":seq": seq,
                                },
                            )
                            .await?;
                    } else {
                        transaction
                            .execute(
                                "INSERT INTO update_state VALUES (:pts, 0, :date, :seq)",
                                named_params! {
                                    ":pts": pts,
                                    ":date": date,
                                    ":seq": seq,
                                },
                            )
                            .await?;
                    }
                }
                UpdateState::Secondary { qts } => {
                    let previous = db
                        .fetch_one(
                            "SELECT * FROM update_state LIMIT 1",
                            named_params![],
                            |_| Ok(()),
                        )
                        .await?;

                    if previous.is_some() {
                        transaction
                            .execute(
                                "UPDATE update_state SET qts = :qts",
                                named_params! {":qts": qts},
                            )
                            .await?;
                    } else {
                        transaction
                            .execute(
                                "INSERT INTO update_state VALUES (0, :qts, 0, 0)",
                                named_params! {":qts": qts},
                            )
                            .await?;
                    }
                }
                UpdateState::Channel { id, pts } => {
                    transaction
                        .execute(
                            "INSERT OR REPLACE INTO channel_state VALUES (:peer_id, :pts)",
                            named_params! {
                                ":peer_id": id,
                                ":pts": pts,
                            },
                        )
                        .await?;
                }
            }

            transaction.commit().await?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};

    use {DcOption, KNOWN_DC_OPTIONS, PeerInfo, Session, UpdateState};

    use super::*;

    #[test]
    fn exercise_sqlite_session() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(do_exercise_sqlite_session());
    }

    async fn do_exercise_sqlite_session() {
        let session = SqliteSession::open(":memory:").await.unwrap();

        assert_eq!(session.home_dc_id().unwrap(), DEFAULT_DC);
        session.set_home_dc_id(DEFAULT_DC + 1).await.unwrap();
        assert_eq!(session.home_dc_id().unwrap(), DEFAULT_DC + 1);

        assert_eq!(
            session.dc_option(KNOWN_DC_OPTIONS[0].id).unwrap(),
            Some(KNOWN_DC_OPTIONS[0].clone())
        );
        let new_dc_option = DcOption {
            id: KNOWN_DC_OPTIONS
                .iter()
                .map(|dc_option| dc_option.id)
                .max()
                .unwrap()
                + 1,
            ipv4: SocketAddrV4::new(Ipv4Addr::from_bits(0), 1),
            ipv6: SocketAddrV6::new(Ipv6Addr::from_bits(0), 1, 0, 0),
            auth_key: Some([1; 256]),
        };
        assert_eq!(session.dc_option(new_dc_option.id).unwrap(), None);
        session.set_dc_option(&new_dc_option).await.unwrap();
        assert_eq!(
            session.dc_option(new_dc_option.id).unwrap(),
            Some(new_dc_option)
        );

        assert_eq!(session.peer(PeerId::self_user()).await.unwrap(), None);
        assert_eq!(session.peer(PeerId::user_unchecked(1)).await.unwrap(), None);
        let peer = PeerInfo::User {
            id: 1,
            auth: None,
            bot: Some(true),
            is_self: Some(true),
        };
        session.cache_peer(&peer).await.unwrap();
        assert_eq!(
            session.peer(PeerId::self_user()).await.unwrap(),
            Some(peer.clone())
        );
        assert_eq!(
            session.peer(PeerId::user_unchecked(1)).await.unwrap(),
            Some(peer)
        );

        assert_eq!(
            session.peer(PeerId::channel_unchecked(1)).await.unwrap(),
            None
        );
        let peer = PeerInfo::Channel {
            id: 1,
            auth: Some(PeerAuth::from_hash(-1)),
            kind: Some(ChannelKind::Broadcast),
        };
        session.cache_peer(&peer).await.unwrap();
        assert_eq!(
            session.peer(PeerId::channel_unchecked(1)).await.unwrap(),
            Some(peer)
        );

        assert_eq!(
            session.updates_state().await.unwrap(),
            UpdatesState::default()
        );
        session
            .set_update_state(UpdateState::All(UpdatesState {
                pts: 1,
                qts: 2,
                date: 3,
                seq: 4,
                channels: vec![
                    ChannelState { id: 5, pts: 6 },
                    ChannelState { id: 7, pts: 8 },
                ],
            }))
            .await
            .unwrap();
        session
            .set_update_state(UpdateState::Primary {
                pts: 2,
                date: 4,
                seq: 5,
            })
            .await
            .unwrap();
        session
            .set_update_state(UpdateState::Secondary { qts: 3 })
            .await
            .unwrap();
        session
            .set_update_state(UpdateState::Channel { id: 7, pts: 9 })
            .await
            .unwrap();
        assert_eq!(
            session.updates_state().await.unwrap(),
            UpdatesState {
                pts: 2,
                qts: 3,
                date: 4,
                seq: 5,
                channels: vec![
                    ChannelState { id: 5, pts: 6 },
                    ChannelState { id: 7, pts: 9 },
                ],
            }
        );
    }
}
