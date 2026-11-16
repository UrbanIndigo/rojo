//! State that lets AI agents, through `rojo mcp`, ask a Studio client to
//! connect to a serve session and see whether one did.
//!
//! Rojo cannot reach into Studio on its own. Instead, an agent records a
//! connection request here, and a Rojo plugin that has opted into agent
//! connections polls for it and connects like a user clicking "Connect".

use std::{
    sync::{
        atomic::{AtomicU32, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

/// How long a connection request stays pending. A plugin that has not picked
/// it up by then is most likely not polling, and connecting much later would
/// surprise whoever is at the keyboard.
pub const CONNECT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// A request from an agent for a Studio client to connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectRequest {
    /// Increments with every request in a serve session, so a plugin can tell
    /// a new request from one it already handled.
    pub id: u32,

    /// Only a Studio client with this place ID should respond, when set.
    pub place_id: Option<u64>,
}

#[derive(Debug)]
struct PendingRequest {
    request: ConnectRequest,
    created: Instant,
}

#[derive(Debug, Default)]
pub struct AgentState {
    connected_clients: Arc<AtomicUsize>,
    last_request_id: AtomicU32,
    pending: Mutex<Option<PendingRequest>>,
}

impl AgentState {
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of clients that currently receive live updates.
    pub fn connected_clients(&self) -> usize {
        self.connected_clients.load(Ordering::SeqCst)
    }

    /// Records that a client started receiving live updates. The client counts
    /// as connected until the returned guard is dropped.
    ///
    /// A connecting client answers any pending connection request, so that
    /// other Studio windows polling the same server do not connect too.
    pub fn client_connected(&self) -> ConnectedClientGuard {
        self.connected_clients.fetch_add(1, Ordering::SeqCst);
        *self.pending.lock().unwrap() = None;

        ConnectedClientGuard {
            connected_clients: Arc::clone(&self.connected_clients),
        }
    }

    /// Asks a Studio client to connect, replacing any earlier request.
    pub fn request_connection(&self, place_id: Option<u64>) -> ConnectRequest {
        self.request_connection_at(place_id, Instant::now())
    }

    fn request_connection_at(&self, place_id: Option<u64>, now: Instant) -> ConnectRequest {
        let request = ConnectRequest {
            id: self.last_request_id.fetch_add(1, Ordering::SeqCst) + 1,
            place_id,
        };

        *self.pending.lock().unwrap() = Some(PendingRequest {
            request,
            created: now,
        });

        request
    }

    /// The connection request that is waiting for a client, if any.
    pub fn pending_request(&self) -> Option<ConnectRequest> {
        self.pending_request_at(Instant::now())
    }

    fn pending_request_at(&self, now: Instant) -> Option<ConnectRequest> {
        let mut pending = self.pending.lock().unwrap();

        match &*pending {
            Some(entry) if now.duration_since(entry.created) < CONNECT_REQUEST_TIMEOUT => {
                Some(entry.request)
            }
            Some(_) => {
                *pending = None;
                None
            }
            None => None,
        }
    }
}

/// Counts a client as connected for as long as it is alive.
#[must_use]
pub struct ConnectedClientGuard {
    connected_clients: Arc<AtomicUsize>,
}

impl Drop for ConnectedClientGuard {
    fn drop(&mut self) {
        self.connected_clients.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn connected_clients_are_counted_until_their_guard_drops() {
        let state = AgentState::new();
        assert_eq!(state.connected_clients(), 0);

        let first = state.client_connected();
        let second = state.client_connected();
        assert_eq!(state.connected_clients(), 2);

        drop(first);
        assert_eq!(state.connected_clients(), 1);
        drop(second);
        assert_eq!(state.connected_clients(), 0);
    }

    #[test]
    fn connection_requests_get_increasing_ids_and_replace_each_other() {
        let state = AgentState::new();
        assert_eq!(state.pending_request(), None);

        let first = state.request_connection(None);
        let second = state.request_connection(Some(1234));

        assert!(second.id > first.id);
        assert_eq!(state.pending_request(), Some(second));
        assert_eq!(state.pending_request().unwrap().place_id, Some(1234));
    }

    #[test]
    fn a_connecting_client_answers_the_pending_request() {
        let state = AgentState::new();
        state.request_connection(None);

        let _client = state.client_connected();

        assert_eq!(state.pending_request(), None);
    }

    #[test]
    fn connection_requests_expire() {
        let state = AgentState::new();
        let start = Instant::now();
        let request = state.request_connection_at(None, start);

        assert_eq!(
            state.pending_request_at(start + CONNECT_REQUEST_TIMEOUT - Duration::from_secs(1)),
            Some(request)
        );
        assert_eq!(
            state.pending_request_at(start + CONNECT_REQUEST_TIMEOUT),
            None
        );
        // Expired requests are cleared rather than coming back later.
        assert_eq!(state.pending_request_at(start), None);
    }
}
