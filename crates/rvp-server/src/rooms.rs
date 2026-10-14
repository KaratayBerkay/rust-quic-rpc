//! Room registry + SFU fanout bookkeeping (plan §4).
//!
//! Hot-path rules:
//! - Room state lookups are lock-cheap; locks are never held across `.await`
//! - Fanout clones `quinn::Connection` handles (Arc) and `Bytes` (refcount),
//!   never media buffers

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use dashmap::DashMap;
use quinn::Connection;
use rvp_proto::control::RosterEntry;
use tokio::sync::Mutex;
use tracing::debug;

/// Hard room cap for P3 (raised/config later).
pub const MAX_ROOM_PARTICIPANTS: usize = 8;

pub struct Participant {
    pub user_id: String,
    pub conn: Connection,
    /// Control stream kept open for server-push events (ParticipantJoined/Left).
    pub control: Arc<Mutex<quinn::SendStream>>,
}

pub struct Room {
    participants: RwLock<HashMap<u64, Participant>>,
}

impl Room {
    fn new() -> Self {
        Self {
            participants: RwLock::new(HashMap::new()),
        }
    }

    /// Insert participant; returns roster of *other* members (excluding self).
    pub(crate) fn insert(
        &self,
        participant_id: u64,
        participant: Participant,
    ) -> Result<Vec<RosterEntry>, ()> {
        let mut map = self.participants.write().expect("room lock poisoned");
        if map.len() >= MAX_ROOM_PARTICIPANTS {
            return Err(());
        }
        let roster = map
            .iter()
            .map(|(pid, p)| RosterEntry {
                participant_id: *pid,
                user_id: p.user_id.clone(),
            })
            .collect();
        map.insert(participant_id, participant);
        Ok(roster)
    }

    pub(crate) fn remove(&self, participant_id: u64) -> Option<Participant> {
        self.participants
            .write()
            .expect("room lock poisoned")
            .remove(&participant_id)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.participants
            .read()
            .expect("room lock poisoned")
            .is_empty()
    }

    /// Snapshot of *other* connections for fanout (lock released before send).
    pub(crate) fn fanout_targets(&self, from: u64) -> Vec<Connection> {
        self.participants
            .read()
            .expect("room lock poisoned")
            .iter()
            .filter(|(pid, _)| **pid != from)
            .map(|(_, p)| p.conn.clone())
            .collect()
    }

    /// Snapshot of *other* control streams for event broadcast.
    pub(crate) fn event_targets(&self, except: u64) -> Vec<(u64, Arc<Mutex<quinn::SendStream>>)> {
        self.participants
            .read()
            .expect("room lock poisoned")
            .iter()
            .filter(|(pid, _)| **pid != except)
            .map(|(pid, p)| (*pid, p.control.clone()))
            .collect()
    }
}

/// Process-wide room registry keyed by room_id (from verified grants).
pub struct RoomRegistry {
    rooms: DashMap<String, Arc<Room>>,
}

impl RoomRegistry {
    pub fn new() -> Self {
        Self {
            rooms: DashMap::new(),
        }
    }

    /// Register a participant; returns (room, roster of others).
    pub fn join(
        &self,
        room_id: &str,
        participant_id: u64,
        user_id: &str,
        conn: Connection,
        control: Arc<Mutex<quinn::SendStream>>,
    ) -> Result<(Arc<Room>, Vec<RosterEntry>), RoomError> {
        let room = self
            .rooms
            .entry(room_id.to_owned())
            .or_insert_with(|| Arc::new(Room::new()))
            .clone();
        let roster = room
            .insert(
                participant_id,
                Participant {
                    user_id: user_id.to_owned(),
                    conn,
                    control,
                },
            )
            .map_err(|_| RoomError::Full)?;
        Ok((room, roster))
    }

    /// Remove a participant and garbage-collect empty rooms.
    /// Returns the room (for post-removal event broadcast).
    pub fn leave(&self, room_id: &str, participant_id: u64) -> Option<Arc<Room>> {
        let room = self.rooms.get(room_id)?.clone();
        room.remove(participant_id);
        if room.is_empty() {
            self.rooms.remove(room_id);
            debug!(room_id, "room gc (empty)");
        }
        Some(room)
    }
}

impl Default for RoomRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RoomError {
    #[error("room is full")]
    Full,
}
