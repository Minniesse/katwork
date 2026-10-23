use crate::{
    error::KatError,
    ids::{EventSeq, ThreadId, UserId},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;

pub const MAX_STREAMS_PER_SESSION: usize = 4;
pub const MAX_STREAMS_PER_USER: usize = 8;

#[derive(Debug, Default)]
struct State {
    senders: HashMap<ThreadId, broadcast::Sender<EventSeq>>,
    sessions: HashMap<String, usize>,
    users: HashMap<UserId, usize>,
}

#[derive(Debug, Clone, Default)]
pub struct EventHub {
    state: Arc<Mutex<State>>,
}

impl EventHub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn subscribe(&self, thread: ThreadId) -> HubSubscription {
        let mut state = self.state.lock().unwrap();
        let receiver = state
            .senders
            .entry(thread)
            .or_insert_with(|| broadcast::channel(64).0)
            .subscribe();
        HubSubscription {
            hub: self.clone(),
            thread,
            receiver: Some(receiver),
        }
    }

    pub fn notify(&self, thread: ThreadId, seq: EventSeq) {
        if let Some(sender) = self.state.lock().unwrap().senders.get(&thread) {
            let _ = sender.send(seq);
        }
    }

    pub fn sender_count(&self) -> usize {
        self.state.lock().unwrap().senders.len()
    }

    /// `session` is the SHA-256 token digest, never the cookie token itself.
    pub fn open_stream(&self, session: &str) -> Result<StreamPermit, KatError> {
        self.reserve_stream(session, None)
    }

    /// Reserve both budgets atomically for an authenticated HTTP stream.
    pub fn open_stream_for_user(
        &self,
        session: &str,
        user: UserId,
    ) -> Result<StreamPermit, KatError> {
        self.reserve_stream(session, Some(user))
    }

    fn reserve_stream(
        &self,
        session: &str,
        user: Option<UserId>,
    ) -> Result<StreamPermit, KatError> {
        let mut state = self.state.lock().unwrap();
        if state.sessions.get(session).copied().unwrap_or(0) >= MAX_STREAMS_PER_SESSION
            || user.is_some_and(|user| {
                state.users.get(&user).copied().unwrap_or(0) >= MAX_STREAMS_PER_USER
            })
        {
            return Err(KatError::TooManyStreams);
        }
        *state.sessions.entry(session.to_owned()).or_default() += 1;
        if let Some(user) = user {
            *state.users.entry(user).or_default() += 1;
        }
        Ok(StreamPermit {
            hub: self.clone(),
            session: session.to_owned(),
            user,
        })
    }
}

pub struct HubSubscription {
    hub: EventHub,
    thread: ThreadId,
    receiver: Option<broadcast::Receiver<EventSeq>>,
}

impl HubSubscription {
    pub async fn recv(&mut self) -> Result<EventSeq, broadcast::error::RecvError> {
        self.receiver
            .as_mut()
            .expect("subscription is alive")
            .recv()
            .await
    }
}

impl Drop for HubSubscription {
    fn drop(&mut self) {
        let mut state = self.hub.state.lock().unwrap();
        self.receiver.take();
        if state
            .senders
            .get(&self.thread)
            .is_some_and(|sender| sender.receiver_count() == 0)
        {
            state.senders.remove(&self.thread);
        }
    }
}

#[derive(Debug)]
pub struct StreamPermit {
    hub: EventHub,
    session: String,
    user: Option<UserId>,
}

impl Drop for StreamPermit {
    fn drop(&mut self) {
        let mut state = self.hub.state.lock().unwrap();
        if let Some(count) = state.sessions.get_mut(&self.session) {
            *count -= 1;
            if *count == 0 {
                state.sessions.remove(&self.session);
            }
        }
        if let Some(user) = self.user
            && let Some(count) = state.users.get_mut(&user)
        {
            *count -= 1;
            if *count == 0 {
                state.users.remove(&user);
            }
        }
    }
}
