use std::{collections::VecDeque, convert::Infallible, sync::Arc, time::Duration};

use axum::{
    extract::{Path, RawQuery, State, rejection::PathRejection},
    http::HeaderMap,
    response::{IntoResponse, Response, Sse, sse::Event},
};
use futures::{Stream, StreamExt};
use katwork::{
    auth::session,
    chat::thread,
    error::KatError,
    events::{HubSubscription, StreamPermit, log},
    ids::{EventSeq, ThreadId},
    runtime::Core,
    tenancy::{self, Tenant},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::time::{Instant, MissedTickBehavior};

use crate::{app::AppState, error::ApiError, extract};

const PAGE_SIZE: u32 = 500;
const POLL: Duration = Duration::from_secs(2);
const PING: Duration = Duration::from_secs(15);
const AUTH: Duration = Duration::from_secs(30);
const SLOW_CLIENT: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
pub struct EventPath {
    thread_id: ThreadId,
}

pub async fn subscribe(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<EventPath>, PathRejection>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Result<Response, ApiError> {
    let thread_id = extract::path(path)?.thread_id;
    thread::get(&state.core.pool, &tenant, thread_id).await?;
    let after = cursor(&headers, query.as_deref())?;
    let token = extract::session_cookie(&headers)
        .ok_or(KatError::Unauthenticated)?
        .to_owned();
    let digest = format!("{:x}", Sha256::digest(token.as_bytes()));
    let permit = match state.core.events.open_stream(&digest) {
        Ok(permit) => permit,
        Err(KatError::TooManyStreams) => {
            return Ok(Sse::new(futures::stream::iter([
                Ok::<_, Infallible>(Event::default().retry(Duration::from_secs(30))),
                Ok(control("stream_error", json!({"code":"too_many_streams"}))),
            ]))
            .into_response());
        }
        Err(error) => return Err(error.into()),
    };
    let subscription = state.core.events.subscribe(thread_id);
    let shutdown = state.core.shutdown.clone();
    let stream = events(
        state.core,
        tenant,
        thread_id,
        token,
        after,
        subscription,
        permit,
    )
    .take_until(shutdown.cancelled_owned());
    Ok(Sse::new(stream).into_response())
}

fn cursor(headers: &HeaderMap, query: Option<&str>) -> Result<EventSeq, KatError> {
    let mut values = headers.get_all("last-event-id").iter();
    let raw = if let Some(header) = values.next() {
        if values.next().is_some() {
            return Err(KatError::InvalidCursor);
        }
        Some(
            header
                .to_str()
                .map_err(|_| KatError::InvalidCursor)?
                .to_owned(),
        )
    } else {
        let mut values = url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
            .filter(|(key, _)| key == "after")
            .map(|(_, value)| value.into_owned());
        let value = values.next();
        if values.next().is_some() {
            return Err(KatError::InvalidCursor);
        }
        value
    };
    let Some(raw) = raw else {
        return Ok(EventSeq(0));
    };
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(KatError::InvalidCursor);
    }
    let seq: i64 = raw.parse().map_err(|_| KatError::InvalidCursor)?;
    if seq > 9_007_199_254_740_991 {
        return Err(KatError::InvalidCursor);
    }
    Ok(EventSeq(seq))
}

fn control(name: &str, data: Value) -> Event {
    Event::default()
        .event(name)
        .json_data(data)
        .expect("control frames are JSON values")
}

fn events(
    core: Arc<Core>,
    tenant: Tenant,
    thread: ThreadId,
    token: String,
    mut last: EventSeq,
    mut subscription: HubSubscription,
    permit: StreamPermit,
) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        let _permit = permit;
        let mut ping_at = Instant::now() + PING;
        let mut auth_at = Instant::now() + AUTH;
        let mut poll = tokio::time::interval_at(Instant::now() + POLL, POLL);
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // Pull-based delivery holds one page. A stalled consumer closes and resumes from its last id.
        let mut yielded_at = Instant::now();
        yield Ok(Event::default().retry(Duration::from_secs(3)));
        match log::tail(&core.pool, &tenant, thread).await {
            Ok(tail) if last > tail => {
                yield Ok(control("resync_required", json!({"reason":"cursor_ahead"})));
                return;
            }
            Err(error) => { yield Ok(control("stream_error", json!({"code":error.code()}))); return; }
            _ => {},
        }
        let mut page = VecDeque::new();
        let mut read = true;
        loop {
            if yielded_at.elapsed() >= SLOW_CLIENT {
                yield Ok(control("stream_error", json!({"code":"slow_client"})));
                return;
            }
            if Instant::now() >= auth_at {
                let access = async {
                    let user = session::authenticate(&core.pool, &token).await?;
                    tenancy::resolve(&core.pool, user.user_id, tenant.workspace_id()).await
                }.await;
                if let Err(error) = access {
                    yield Ok(control("stream_error", json!({"code":error.code()})));
                    return;
                }
                auth_at = Instant::now() + AUTH;
            }
            if Instant::now() >= ping_at {
                ping_at = Instant::now() + PING;
                yielded_at = Instant::now();
                yield Ok(control("ping", json!({"tail":last.0})).comment("keep-alive"));
                continue;
            }
            if let Some((seq, event)) = page.pop_front() {
                if seq > last {
                    last = seq;
                    yielded_at = Instant::now();
                    yield Ok(Event::default().id(seq.to_string()).json_data(event).expect("log event is JSON"));
                }
                continue;
            }
            if read {
                match log::since(&core.pool, &tenant, thread, last, PAGE_SIZE).await {
                    Ok(events) => { read = events.len() == PAGE_SIZE as usize; page = events.into(); }
                    Err(error) => { yield Ok(control("stream_error", json!({"code":error.code()}))); return; }
                }
                continue;
            }
            tokio::select! {
                _ = subscription.recv() => read = true,
                _ = poll.tick() => read = true,
                _ = tokio::time::sleep_until(ping_at) => {},
                _ = tokio::time::sleep_until(auth_at) => {},
            }
        }
    }
}
