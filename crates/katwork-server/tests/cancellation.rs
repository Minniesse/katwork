#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; cancellation probes deliberately drop futures"
)]
#[path = "../../katwork/tests/support/cancel.rs"]
mod cancel;
#[allow(dead_code, reason = "Shared HTTP fixture helpers")]
mod support;

use std::{future::Future, sync::Arc};

use axum::{Router, response::Response};
use katwork::{auth, ids::UserId, runtime::Core, tenancy::workspace};
use katwork_server::app::{AppState, build_router};
use serde_json::json;
use sqlx::{PgPool, postgres::PgPoolOptions};
use support::*;

async fn single(pool: &PgPool) -> PgPool {
    PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap()
}

async fn fixture(pool: &PgPool) -> (PgPool, Router, String, String, i32) {
    let (user, _) = user(pool).await;
    let workspace = workspace::create(pool, "Workspace", UserId(user))
        .await
        .unwrap();
    let session = auth::session::create(pool, UserId(user)).await.unwrap();
    let cookie = format!("kw_session={}", session.0);
    let single = single(pool).await;
    let mut config = config(None);
    config.lock_timeout = std::time::Duration::from_millis(100);
    let app = build_router(AppState {
        core: Arc::new(Core::new(single.clone(), Arc::new(config))),
    });
    let pid = cancel::backend_pid(&single).await;
    (single, app, cookie, workspace.to_string(), pid)
}

async fn drop_waiting(pool: &PgPool, pid: i32, future: impl Future<Output = Response>) {
    let mut future = Box::pin(future);
    tokio::select! {
        response = future.as_mut() => panic!("request escaped barrier: {}", response.status()),
        () = cancel::wait_for_lock(pool, pid) => {},
    }
    cancel::poll_once_then_drop(future).await;
}

async fn assert_closed(observer: &PgPool, single: &PgPool, pid: i32) {
    assert_ne!(
        cancel::backend_pid(single).await,
        pid,
        "cancelled request reused its backend"
    );
    cancel::assert_backend_idle(observer, pid).await;
    single.close().await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancelled_auth_transaction_closes_connection(pool: PgPool) {
    let (id, password) = user(&pool).await;
    let single = single(&pool).await;
    let app = app(&single, None);
    let pid = cancel::backend_pid(&single).await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR NO KEY UPDATE")
        .bind(id)
        .execute(&mut *blocker)
        .await
        .unwrap();
    drop_waiting(&pool, pid, login(&app, &password)).await;
    blocker.rollback().await.unwrap();
    assert_closed(&pool, &single, pid).await;
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sessions, 0);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancelled_workspace_lock_closes_connection(pool: PgPool) {
    let (single, app, cookie, workspace, pid) = fixture(&pool).await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM workspaces WHERE id::text=$1 FOR NO KEY UPDATE")
        .bind(&workspace)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let path = format!(
        "/api/workspaces/{workspace}/spaces/{}/kats",
        uuid::Uuid::now_v7()
    );
    drop_waiting(
        &pool,
        pid,
        request(
            &app,
            "POST",
            &path,
            Some(json!({"name":"Researcher","role":"researcher","persona":""})),
            Some(&cookie),
        ),
    )
    .await;
    blocker.rollback().await.unwrap();
    assert_closed(&pool, &single, pid).await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancelled_tenancy_create_closes_connection(pool: PgPool) {
    let (single, app, cookie, _, pid) = fixture(&pool).await;
    let mut blocker = pool.begin().await.unwrap();
    // Table locks park the INSERT while allowing authentication's SELECTs.
    sqlx::query("LOCK workspaces IN SHARE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    drop_waiting(
        &pool,
        pid,
        request(
            &app,
            "POST",
            "/api/workspaces",
            Some(json!({"name":"Cancelled"})),
            Some(&cookie),
        ),
    )
    .await;
    blocker.rollback().await.unwrap();
    assert_closed(&pool, &single, pid).await;
    let created: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workspaces WHERE name='Cancelled')")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!created);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancelled_single_statement_closes_connection(pool: PgPool) {
    let (single, app, cookie, workspace, pid) = fixture(&pool).await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM workspaces WHERE id::text=$1 FOR NO KEY UPDATE")
        .bind(&workspace)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let path = format!("/api/workspaces/{workspace}/paused");
    drop_waiting(
        &pool,
        pid,
        request(
            &app,
            "PUT",
            &path,
            Some(json!({"paused":true})),
            Some(&cookie),
        ),
    )
    .await;
    blocker.rollback().await.unwrap();
    assert_closed(&pool, &single, pid).await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn normal_validation_reuses_connection(pool: PgPool) {
    let (single, app, cookie, workspace, pid) = fixture(&pool).await;
    // Validation in message intake happens after BEGIN, exercising the guard's error path.
    let message_path = format!(
        "/api/workspaces/{workspace}/threads/{}/messages",
        uuid::Uuid::now_v7()
    );
    assert_code(
        request(
            &app,
            "POST",
            &message_path,
            Some(json!({"body":"","client_msg_id":"cancel-test"})),
            Some(&cookie),
        )
        .await,
        422,
        "message_empty",
    )
    .await;
    assert_eq!(
        cancel::backend_pid(&single).await,
        pid,
        "422 burned a healthy connection"
    );
    cancel::assert_backend_idle(&pool, pid).await;
    single.close().await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn normal_busy_reuses_connection(pool: PgPool) {
    let (single, app, cookie, workspace, pid) = fixture(&pool).await;
    let path = format!("/api/workspaces/{workspace}/kats/{}", uuid::Uuid::now_v7());
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM workspaces WHERE id::text=$1 FOR NO KEY UPDATE")
        .bind(&workspace)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let response = tokio::time::timeout(
        cancel::DEADLINE,
        request(
            &app,
            "PATCH",
            &path,
            Some(json!({"name":"Changed"})),
            Some(&cookie),
        ),
    )
    .await
    .expect("unbounded workspace lock");
    assert_eq!(response.headers().get("retry-after").unwrap(), "1");
    assert_code(response, 503, "busy").await;
    assert_eq!(
        cancel::backend_pid(&single).await,
        pid,
        "503 burned a healthy connection"
    );
    cancel::assert_backend_idle(&pool, pid).await;
    blocker.rollback().await.unwrap();
    single.close().await;
}
