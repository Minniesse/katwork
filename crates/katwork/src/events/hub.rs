use crate::{
    error::KatError,
    ids::{EventSeq, ThreadId},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;

pub const MAX_STREAMS_PER_SESSION: usize = 4;

#[derive(Debug, Default)]
struct State {
    senders: HashMap<ThreadId, broadcast::Sender<EventSeq>>,
    sessions: HashMap<String, usize>,
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
        let mut state = self.state.lock().unwrap();
        let count = state.sessions.entry(session.to_owned()).or_default();
        if *count == MAX_STREAMS_PER_SESSION {
            return Err(KatError::TooManyStreams);
        }
        *count += 1;
        Ok(StreamPermit {
            hub: self.clone(),
            session: session.to_owned(),
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
    }
}
