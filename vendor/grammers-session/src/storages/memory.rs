// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::fmt;
use std::sync::{Mutex, MutexGuard};

use crate::types::{ChannelState, DcOption, PeerId, PeerInfo, UpdateState, UpdatesState};
use crate::{BoxFuture, Session, SessionData};

/// In-memory session interface.
///
/// Does not actually offer direct ways to persist the state anywhere,
/// so it should only be used in very few select cases.
///
/// Logging in has a very high cost in terms of flood wait errors,
/// so the state really should be persisted by other means.
#[derive(Default)]
pub struct MemorySession(Mutex<SessionData>);

impl From<SessionData> for MemorySession {
    /// Constructs a memory session from the entirety of the session data,
    /// unlike the blanket `From` implementation which cannot import all values
    fn from(session_data: SessionData) -> Self {
        Self(Mutex::new(session_data))
    }
}

#[derive(Debug)]
pub enum MemorySessionError {
    Poisoned,
}

impl std::error::Error for MemorySessionError {}

impl fmt::Display for MemorySessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemorySessionError::Poisoned => write!(f, "session lock is poisoned"),
        }
    }
}

impl MemorySession {
    /// Lock the session data, mapping a poisoned mutex into [`MemorySessionError`].
    fn data(&self) -> Result<MutexGuard<'_, SessionData>, MemorySessionError> {
        self.0.lock().map_err(|_| MemorySessionError::Poisoned)
    }
}

impl Session for MemorySession {
    type Error = MemorySessionError;

    fn home_dc_id(&self) -> Result<i32, MemorySessionError> {
        Ok(self.data()?.home_dc)
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), MemorySessionError>> {
        Box::pin(async move {
            self.data()?.home_dc = dc_id;
            Ok(())
        })
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, MemorySessionError> {
        Ok(self.data()?.dc_options.get(&dc_id).cloned())
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, Result<(), MemorySessionError>> {
        let dc_option = dc_option.clone();
        Box::pin(async move {
            self.data()?
                .dc_options
                .insert(dc_option.id, dc_option.clone());
            Ok(())
        })
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, MemorySessionError>> {
        Box::pin(async move { Ok(self.data()?.peer_infos.get(&peer).cloned()) })
    }

    fn cache_peer(&self, peer: &PeerInfo) -> BoxFuture<'_, Result<(), MemorySessionError>> {
        let peer = peer.clone();
        Box::pin(async move {
            self.data()?
                .peer_infos
                .entry(peer.id())
                .or_insert_with(|| peer.clone())
                .extend_info(&peer);
            Ok(())
        })
    }

    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, MemorySessionError>> {
        Box::pin(async move { Ok(self.data()?.updates_state.clone()) })
    }

    fn set_update_state(
        &self,
        update: UpdateState,
    ) -> BoxFuture<'_, Result<(), MemorySessionError>> {
        Box::pin(async move {
            let mut data = self.data()?;

            match update {
                UpdateState::All(updates_state) => {
                    data.updates_state = updates_state;
                }
                UpdateState::Primary { pts, date, seq } => {
                    data.updates_state.pts = pts;
                    data.updates_state.date = date;
                    data.updates_state.seq = seq;
                }
                UpdateState::Secondary { qts } => {
                    data.updates_state.qts = qts;
                }
                UpdateState::Channel { id, pts } => {
                    data.updates_state.channels.retain(|c| c.id != id);
                    data.updates_state.channels.push(ChannelState { id, pts });
                }
            }

            Ok(())
        })
    }
}
