use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use trogon_presence::ConnectionId;

use super::limits::SnapshotLimits;
use super::wire::SnapshotBytes;

#[derive(Debug, Default)]
struct GateState {
    per_connection: HashMap<ConnectionId, usize>,
    active: usize,
    reserved: usize,
}

#[derive(Debug, Clone)]
pub struct AssemblyGate {
    state: Arc<Mutex<GateState>>,
    limits: SnapshotLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AssemblyRefusal {
    #[error("connection already has the maximum concurrent snapshot assemblies")]
    Connection,
    #[error("process already has the maximum concurrent snapshot assemblies")]
    Process,
    #[error("snapshot assembly byte budget is exhausted")]
    Budget,
}

impl AssemblyGate {
    pub fn new(limits: SnapshotLimits) -> Self {
        Self {
            state: Arc::default(),
            limits,
        }
    }

    pub fn admit(&self, connection: &ConnectionId, bytes: SnapshotBytes) -> Result<AssemblyPermit, AssemblyRefusal> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let held = state.per_connection.get(connection).copied().unwrap_or_default();
        if held >= self.limits.per_connection().get() {
            return Err(AssemblyRefusal::Connection);
        }
        if state.active >= self.limits.per_process().get() {
            return Err(AssemblyRefusal::Process);
        }
        let reservation = bytes.reservation();
        let remaining = self.limits.budget().get().saturating_sub(state.reserved);
        if reservation > remaining {
            return Err(AssemblyRefusal::Budget);
        }
        state.reserved += reservation;
        state.active += 1;
        *state.per_connection.entry(*connection).or_default() += 1;
        Ok(AssemblyPermit {
            state: self.state.clone(),
            connection: *connection,
            reservation,
        })
    }
}

#[derive(Debug)]
pub struct AssemblyPermit {
    state: Arc<Mutex<GateState>>,
    connection: ConnectionId,
    reservation: usize,
}

impl Drop for AssemblyPermit {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.reserved = state.reserved.saturating_sub(self.reservation);
        state.active = state.active.saturating_sub(1);
        if let Some(held) = state.per_connection.get_mut(&self.connection) {
            *held = held.saturating_sub(1);
            if *held == 0 {
                state.per_connection.remove(&self.connection);
            }
        }
    }
}
