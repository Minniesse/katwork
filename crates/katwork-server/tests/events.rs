#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; cancellation probes deliberately drop futures"
)]
#[path = "../../katwork/tests/support/cancel.rs"]
mod cancel;
#[path = "../../katwork/tests/support/mod.rs"]
#[allow(dead_code)]
mod data;
#[path = "events/framing.rs"]
mod framing;
#[allow(dead_code)]
mod support;

use axum::{
    Router,
    body::{Body, BodyDataStream, Bytes},
    http::Request,
    response::Response,
};
use futures::StreamExt;
use katwork::{
    auth::session,
    events::{AgUiEvent, EventSink, PgEventSink, log},
    ids::*,
    runtime::Core,
};
use katwork_server::app::{AppState, build_router};
use sqlx::PgPool;
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

struct Setup {
    core: Arc<Core>,
    app: Router,
    data: data::Fixture,
    token: String,
}

impl Setup {
    async fn new(pool: &PgPool) -> Self {
        let data = data::Fixture::new(pool).await;
        let token = session::create(pool, UserId(data.user)).await.unwrap().0;
        let core = Arc::new(Core::new(pool.clone(), Arc::new(support::config(None))));
        let app = build_router(AppState { core: core.clone() });
        Self {
            core,
            app,
            data,
            token,
        }
    }
    fn path(&self) -> String {
        format!(
            "/api/workspaces/{}/threads/{}/events",
            self.data.workspace, self.data.thread
        )
    }
    async fn response(&self, query: &str, cursor: Option<&str>) -> Response {
        let mut req = Request::builder()
            .uri(format!("{}{query}", self.path()))
            .header("host", "localhost")
            .header("cookie", format!("kw_session={}", self.token));
        if let Some(cursor) = cursor {
            req = req.header("last-event-id", cursor);
        }
        self.app
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }
    async fn stream(&self, cursor: Option<&str>) -> BodyDataStream {
        let response = self.response("", cursor).await;
        assert_eq!(response.status(), 200);
        response.into_body().into_data_stream()
    }
    async fn append(&self, n: usize, notify: bool) {
        let mut tx = self.core.pool.begin().await.unwrap();
        let mut seq = EventSeq(0);
        for _ in 0..n {
            seq = log::append(
                &mut tx,
                WorkspaceId(self.data.workspace),
                ThreadId(self.data.thread),
                None,
                &AgUiEvent::TextMessageContent {
                    message_id: "reply".into(),
                    delta: "สวัสดี 👋🏽".into(),
                },
            )
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
        if notify {
            self.core.events.notify(ThreadId(self.data.thread), seq);
        }
    }
}

async fn next(stream: &mut BodyDataStream) -> Bytes {
    tokio::time::timeout(Duration::from_secs(60), stream.next())
        .await
        .expect("stream stalled")
        .expect("stream ended")
        .unwrap()
}

fn has(bytes: &[u8], text: &str) -> bool {
    bytes
        .windows(text.len())
        .any(|window| window == text.as_bytes())
}

async fn seq(stream: &mut BodyDataStream) -> i64 {
    loop {
        let chunk = next(stream).await;
        for line in chunk.split(|b| *b == b'\n') {
            if let Some(id) = line
                .strip_prefix(b"id: ")
                .or_else(|| line.strip_prefix(b"id:"))
            {
                return std::str::from_utf8(id).unwrap().trim().parse().unwrap();
            }
        }
    }
}

async fn assert_end(stream: &mut BodyDataStream) {
    assert!(
        tokio::time::timeout(Duration::from_secs(60), stream.next())
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn dropped_sse_read_returns_idle_backend_and_releases_subscription(pool: PgPool) {
    let single = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let s = Setup::new(&single).await;
    let mut stream = s.stream(None).await;
    assert!(has(&next(&mut stream).await, "retry:"));
    let pid = cancel::backend_pid(&single).await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("LOCK threads IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let mut reading = Box::pin(stream.next());
    tokio::select! {
        _ = reading.as_mut() => panic!("SSE read escaped its barrier"),
        () = cancel::wait_for_lock(&pool, pid) => {},
    }
    drop(reading);
    drop(stream);
    assert_eq!(s.core.events.sender_count(), 0);
    blocker.rollback().await.unwrap();
    assert_eq!(cancel::backend_pid(&single).await, pid);
    cancel::assert_backend_idle(&pool, pid).await;
    single.close().await;
}

// Keep PostgreSQL's real I/O from triggering Tokio's automatic virtual-time jumps.
struct Clock(tokio::task::JoinHandle<()>);
impl Clock {
    fn pause() -> Self {
        tokio::time::pause();
        Self(tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        }))
    }
}
impl Drop for Clock {
    fn drop(&mut self) {
        self.0.abort();
        tokio::time::resume();
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn reconnect_with_last_event_id_has_no_gap_or_duplicate(pool: PgPool) {
    let s = Setup::new(&pool).await;
    s.append(20, true).await;
    let mut stream = s.stream(None).await;
    for expected in 1..=7 {
        assert_eq!(seq(&mut stream).await, expected);
    }
    drop(stream);
    s.append(20, true).await;
    let mut stream = s.stream(Some("7")).await;
    for expected in 8..=40 {
        assert_eq!(seq(&mut stream).await, expected);
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn reconnect_during_a_burst(pool: PgPool) {
    let s = Setup::new(&pool).await;
    let sink = PgEventSink::new(
        pool,
        s.core.events.clone(),
        Duration::from_secs(5),
        WorkspaceId(s.data.workspace),
        ThreadId(s.data.thread),
        None,
        None,
    );
    let writer = tokio::spawn(async move {
        for _ in 0..200 {
            sink.emit(AgUiEvent::TextMessageContent {
                message_id: "m".into(),
                delta: "🇹🇭".into(),
            })
            .await
            .unwrap();
        }
    });
    let mut stream = s.stream(None).await;
    for expected in 1..=37 {
        assert_eq!(seq(&mut stream).await, expected);
    }
    drop(stream);
    let mut stream = s.stream(Some("37")).await;
    for expected in 38..=200 {
        assert_eq!(seq(&mut stream).await, expected);
    }
    writer.await.unwrap();
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn replay_to_live_gap_loses_nothing(pool: PgPool) {
    let s = Setup::new(&pool).await;
    s.append(1, true).await;
    let mut stream = s.stream(None).await;
    assert_eq!(seq(&mut stream).await, 1);
    // The body is suspended at its last replay yield, before it enters the live wait.
    s.append(1, true).await;
    assert_eq!(seq(&mut stream).await, 2);
    s.append(1, true).await;
    assert_eq!(seq(&mut stream).await, 3);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn lagged_subscriber_recovers_from_log(pool: PgPool) {
    let s = Setup::new(&pool).await;
    let mut stream = s.stream(None).await;
    next(&mut stream).await;
    s.append(700, false).await;
    for n in 1..=700 {
        s.core.events.notify(ThreadId(s.data.thread), EventSeq(n));
    }
    for expected in 1..=700 {
        assert_eq!(seq(&mut stream).await, expected);
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn missed_notify_is_recovered_by_poll(pool: PgPool) {
    let s = Setup::new(&pool).await;
    let mut stream = s.stream(None).await;
    s.append(1, true).await;
    assert_eq!(seq(&mut stream).await, 1);
    let _clock = Clock::pause();
    let waiter = tokio::spawn(async move { seq(&mut stream).await });
    tokio::task::yield_now().await;
    s.append(1, false).await;
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(waiter.await.unwrap(), 2);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn last_event_id_header_wins_over_after_query(pool: PgPool) {
    let s = Setup::new(&pool).await;
    s.append(6, false).await;
    let mut stream = s
        .response("?after=2", Some("5"))
        .await
        .into_body()
        .into_data_stream();
    assert_eq!(seq(&mut stream).await, 6);
    let mut stream = s
        .response("?after=2", None)
        .await
        .into_body()
        .into_data_stream();
    assert_eq!(seq(&mut stream).await, 3);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn non_numeric_last_event_id_is_400(pool: PgPool) {
    let s = Setup::new(&pool).await;
    for cursor in [
        "",
        "x",
        "-1",
        "+1",
        "1.0",
        " 1",
        "9007199254740992",
        "999999999999999999999",
    ] {
        support::assert_code(s.response("", Some(cursor)).await, 400, "invalid_cursor").await;
    }
    support::assert_code(s.response("?after=bad", None).await, 400, "invalid_cursor").await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cursor_ahead_of_tail_sends_resync_required_then_ends(pool: PgPool) {
    let s = Setup::new(&pool).await;
    let response = s.response("?after=99", None).await;
    assert_eq!(response.status(), 200);
    let bytes = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    assert!(has(&bytes, "event: resync_required"));
    assert!(has(&bytes, "\"reason\":\"cursor_ahead\""));
    assert!(!has(&bytes, "id:"));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn stream_starts_with_retry_3000(pool: PgPool) {
    let s = Setup::new(&pool).await;
    let mut stream = s.stream(None).await;
    assert_eq!(&next(&mut stream).await[..], b"retry: 3000\n\n");
    drop(stream);
    assert_eq!(s.core.events.sender_count(), 0);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn ping_event_has_no_id_line(pool: PgPool) {
    let s = Setup::new(&pool).await;
    let mut stream = s.stream(None).await;
    next(&mut stream).await;
    let _clock = Clock::pause();
    let waiter = tokio::spawn(async move {
        let bytes = next(&mut stream).await;
        (stream, bytes)
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(15)).await;
    let (mut stream, bytes) = waiter.await.unwrap();
    assert!(has(&bytes, "event: ping"));
    assert!(has(&bytes, ": keep-alive"));
    assert!(has(&bytes, "\"tail\":0"));
    assert!(!has(&bytes, "id:"));
    s.append(1, true).await;
    assert_eq!(seq(&mut stream).await, 1);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn stream_closes_after_session_revoked_or_member_removed(pool: PgPool) {
    for revoke in [true, false] {
        let mut s = Setup::new(&pool).await;
        if !revoke {
            let (viewer, _) = support::user(&pool).await;
            sqlx::query(
                "INSERT INTO memberships (workspace_id,user_id,role) VALUES ($1,$2,'member')",
            )
            .bind(s.data.workspace)
            .bind(viewer)
            .execute(&pool)
            .await
            .unwrap();
            s.data.user = viewer;
            s.token = session::create(&pool, UserId(viewer)).await.unwrap().0;
        }
        let mut stream = s.stream(None).await;
        next(&mut stream).await;
        if revoke {
            session::revoke(&pool, &s.token).await.unwrap();
        } else {
            sqlx::query("DELETE FROM memberships WHERE workspace_id=$1 AND user_id=$2")
                .bind(s.data.workspace)
                .bind(s.data.user)
                .execute(&pool)
                .await
                .unwrap();
        }
        let _clock = Clock::pause();
        tokio::time::advance(Duration::from_secs(30)).await;
        let bytes = next(&mut stream).await;
        assert!(has(&bytes, "event: stream_error"));
        assert!(has(
            &bytes,
            if revoke {
                "unauthenticated"
            } else {
                "not_found"
            }
        ));
        assert!(!has(&bytes, "id:"));
        assert_end(&mut stream).await;
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn fifth_stream_in_a_session_gets_too_many_streams_frame(pool: PgPool) {
    let s = Setup::new(&pool).await;
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(s.stream(None).await);
    }
    let response = s.response("", None).await;
    assert_eq!(response.status(), 200);
    let bytes = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    assert!(bytes.starts_with(b"retry: 30000\n\n"));
    assert!(has(&bytes, "event: stream_error"));
    assert!(has(&bytes, "too_many_streams"));
    assert!(!has(&bytes, "id:"));
    held.pop();
    let mut replacement = s.stream(None).await;
    assert_eq!(&next(&mut replacement).await[..], b"retry: 3000\n\n");
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn ninth_stream_across_user_sessions_gets_too_many_streams_frame(pool: PgPool) {
    let mut s = Setup::new(&pool).await;
    let mut held = Vec::new();
    for _ in 0..2 {
        s.token = session::create(&pool, UserId(s.data.user)).await.unwrap().0;
        for _ in 0..4 {
            let mut stream = s.stream(None).await;
            assert_eq!(&next(&mut stream).await[..], b"retry: 3000\n\n");
            held.push(stream);
        }
    }
    s.token = session::create(&pool, UserId(s.data.user)).await.unwrap().0;
    let mut overflow = s.stream(None).await;
    assert_eq!(&next(&mut overflow).await[..], b"retry: 30000\n\n");
    let bytes = next(&mut overflow).await;
    assert!(has(&bytes, "event: stream_error"));
    assert!(has(&bytes, "too_many_streams"));
    assert!(!has(&bytes, "id:"));
    assert_end(&mut overflow).await;

    // Another account still has its own budget on this same hub.
    let other = data::Fixture::new(&pool).await;
    let token = session::create(&pool, UserId(other.user)).await.unwrap().0;
    let response = support::request(
        &s.app,
        "GET",
        &format!(
            "/api/workspaces/{}/threads/{}/events",
            other.workspace, other.thread
        ),
        None,
        Some(&format!("kw_session={token}")),
    )
    .await;
    let mut independent = response.into_body().into_data_stream();
    assert_eq!(&next(&mut independent).await[..], b"retry: 3000\n\n");

    held.pop();
    let mut replacement = s.stream(None).await;
    assert_eq!(&next(&mut replacement).await[..], b"retry: 3000\n\n");
    drop(held);
    drop(replacement);
    assert_eq!(s.core.events.sender_count(), 1);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn other_workspace_cannot_subscribe(pool: PgPool) {
    let s = Setup::new(&pool).await;
    let other = data::Fixture::new(&pool).await;
    let path = format!(
        "/api/workspaces/{}/threads/{}/events",
        s.data.workspace, other.thread
    );
    support::assert_code(
        support::request(
            &s.app,
            "GET",
            &path,
            None,
            Some(&format!("kw_session={}", s.token)),
        )
        .await,
        404,
        "not_found",
    )
    .await;
    support::assert_code(
        support::request(&s.app, "GET", &s.path(), None, None).await,
        401,
        "unauthenticated",
    )
    .await;
    assert_eq!(s.core.events.sender_count(), 0);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn sse_stream_outlives_the_request_timeout(pool: PgPool) {
    let s = Setup::new(&pool).await;
    let mut stream = s.stream(None).await;
    next(&mut stream).await;
    let _clock = Clock::pause();
    for _ in 0..3 {
        tokio::time::advance(Duration::from_secs(15)).await;
        assert!(has(&next(&mut stream).await, "event: ping"));
    }
    s.append(1, true).await;
    assert_eq!(seq(&mut stream).await, 1);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn open_streams_do_not_count_against_the_request_limit(pool: PgPool) {
    let mut s = Setup::new(&pool).await;
    let mut held = Vec::new();
    for i in 0..257 {
        if i % 8 == 0 {
            s.data = data::Fixture::new(&pool).await;
        }
        if i % 4 == 0 {
            s.token = session::create(&pool, UserId(s.data.user)).await.unwrap().0;
        }
        let mut stream = s.stream(None).await;
        assert_eq!(&next(&mut stream).await[..], b"retry: 3000\n\n");
        held.push(stream);
    }
    assert_eq!(
        support::request(&s.app, "GET", "/healthz", None, None)
            .await
            .status(),
        200
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn sse_route_keeps_host_and_origin_guards(pool: PgPool) {
    let s = Setup::new(&pool).await;
    let req = Request::builder()
        .uri(s.path())
        .header("host", "evil.example")
        .body(Body::empty())
        .unwrap();
    support::assert_code(
        s.app.clone().oneshot(req).await.unwrap(),
        403,
        "host_not_allowed",
    )
    .await;
    let req = Request::builder()
        .method("POST")
        .uri(s.path())
        .header("host", "localhost")
        .header("origin", "https://evil.example")
        .body(Body::empty())
        .unwrap();
    support::assert_code(
        s.app.clone().oneshot(req).await.unwrap(),
        403,
        "cross_site_request",
    )
    .await;
}
